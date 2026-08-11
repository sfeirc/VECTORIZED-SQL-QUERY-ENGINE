use crate::ast::{AggregateFunction, BinaryOp, UnaryOp};
use crate::storage::Table;
use crate::types::{DataType, Field, Schema, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum ScalarExpr {
    Literal(Value),
    Column {
        index: usize,
        qualifier: Option<String>,
        name: String,
        data_type: DataType,
    },
    Unary {
        op: UnaryOp,
        expr: Box<ScalarExpr>,
        data_type: DataType,
    },
    Binary {
        left: Box<ScalarExpr>,
        op: BinaryOp,
        right: Box<ScalarExpr>,
        data_type: DataType,
    },
    Aggregate {
        function: AggregateFunction,
        expr: Box<ScalarExpr>,
        data_type: DataType,
    },
    CountStar,
}

impl ScalarExpr {
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Literal(value) => value.data_type(),
            Self::Column { data_type, .. }
            | Self::Unary { data_type, .. }
            | Self::Binary { data_type, .. }
            | Self::Aggregate { data_type, .. } => *data_type,
            Self::CountStar => DataType::Int64,
        }
    }

    pub fn column_indices(&self, result: &mut Vec<usize>) {
        match self {
            Self::Column { index, .. } => result.push(*index),
            Self::Unary { expr, .. } | Self::Aggregate { expr, .. } => expr.column_indices(result),
            Self::Binary { left, right, .. } => {
                left.column_indices(result);
                right.column_indices(result);
            }
            Self::Literal(_) | Self::CountStar => {}
        }
    }

    pub fn contains_aggregate(&self) -> bool {
        match self {
            Self::Aggregate { .. } | Self::CountStar => true,
            Self::Unary { expr, .. } => expr.contains_aggregate(),
            Self::Binary { left, right, .. } => {
                left.contains_aggregate() || right.contains_aggregate()
            }
            _ => false,
        }
    }
}

impl Display for ScalarExpr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Literal(v) => write!(f, "{v}"),
            Self::Column {
                qualifier: Some(q),
                name,
                ..
            } => write!(f, "{q}.{name}"),
            Self::Column {
                qualifier: None,
                name,
                ..
            } => write!(f, "{name}"),
            Self::Unary { op, expr, .. } => write!(
                f,
                "{} {expr}",
                if *op == UnaryOp::Not { "NOT" } else { "-" }
            ),
            Self::Binary {
                left, op, right, ..
            } => write!(f, "({left} {op} {right})"),
            Self::Aggregate { function, expr, .. } => write!(f, "{:?}({expr})", function),
            Self::CountStar => write!(f, "COUNT(*)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamedExpr {
    pub expr: ScalarExpr,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SortExpr {
    pub expr: ScalarExpr,
    pub asc: bool,
}

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    Scan {
        table: Arc<Table>,
        alias: String,
        read_columns: Vec<usize>,
        predicates: Vec<ScalarExpr>,
    },
    Filter {
        predicate: ScalarExpr,
        input: Box<LogicalPlan>,
    },
    Projection {
        expressions: Vec<NamedExpr>,
        input: Box<LogicalPlan>,
    },
    Aggregate {
        group_by: Vec<ScalarExpr>,
        expressions: Vec<NamedExpr>,
        input: Box<LogicalPlan>,
    },
    Join {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        on: ScalarExpr,
    },
    Sort {
        keys: Vec<SortExpr>,
        input: Box<LogicalPlan>,
    },
    Limit {
        count: usize,
        input: Box<LogicalPlan>,
    },
}

impl LogicalPlan {
    pub fn schema(&self) -> Schema {
        match self {
            Self::Scan { table, alias, .. } => table
                .schema
                .iter()
                .map(|f| Field {
                    qualifier: Some(alias.clone()),
                    name: f.name.clone(),
                    data_type: f.data_type,
                    nullable: f.nullable,
                })
                .collect(),
            Self::Filter { input, .. } | Self::Sort { input, .. } | Self::Limit { input, .. } => {
                input.schema()
            }
            Self::Projection { expressions, .. } | Self::Aggregate { expressions, .. } => {
                expressions
                    .iter()
                    .map(|e| Field {
                        qualifier: None,
                        name: e.name.clone(),
                        data_type: e.expr.data_type(),
                        nullable: true,
                    })
                    .collect()
            }
            Self::Join { left, right, .. } => {
                let mut schema = left.schema();
                schema.extend(right.schema());
                schema
            }
        }
    }

    pub fn estimated_rows(&self) -> usize {
        match self {
            Self::Scan {
                table, predicates, ..
            } => estimate_scan_rows(table, predicates),
            Self::Filter { input, .. } => (input.estimated_rows() / 10).max(1),
            Self::Projection { input, .. } | Self::Sort { input, .. } => input.estimated_rows(),
            Self::Aggregate {
                group_by, input, ..
            } => {
                if group_by.is_empty() {
                    1
                } else {
                    estimate_groups(input, group_by)
                }
            }
            Self::Join { left, right, on } => estimate_join(left, right, on),
            Self::Limit { count, input } => (*count).min(input.estimated_rows()),
        }
    }

    pub fn column_distinct(&self, index: usize) -> Option<usize> {
        match self {
            Self::Scan { table, .. } => table
                .stats
                .columns
                .get(index)
                .map(|statistics| statistics.cardinality),
            Self::Filter { input, .. } | Self::Sort { input, .. } | Self::Limit { input, .. } => {
                input.column_distinct(index)
            }
            Self::Projection {
                expressions, input, ..
            }
            | Self::Aggregate {
                expressions, input, ..
            } => expressions.get(index).and_then(|expression| {
                if let ScalarExpr::Column { index, .. } = expression.expr {
                    input.column_distinct(index)
                } else {
                    None
                }
            }),
            Self::Join { left, right, .. } => {
                let left_width = left.schema().len();
                if index < left_width {
                    left.column_distinct(index)
                } else {
                    right.column_distinct(index - left_width)
                }
            }
        }
    }

    pub fn format_tree(&self) -> String {
        let mut output = String::new();
        self.format_node(&mut output, "", true);
        output
    }

    fn format_node(&self, output: &mut String, prefix: &str, last: bool) {
        output.push_str(prefix);
        output.push_str(if last { "└── " } else { "├── " });
        output.push_str(&self.label());
        output.push('\n');
        let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
        match self {
            Self::Scan { .. } => {}
            Self::Join { left, right, .. } => {
                left.format_node(output, &child_prefix, false);
                right.format_node(output, &child_prefix, true);
            }
            Self::Filter { input, .. }
            | Self::Projection { input, .. }
            | Self::Aggregate { input, .. }
            | Self::Sort { input, .. }
            | Self::Limit { input, .. } => input.format_node(output, &child_prefix, true),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Scan {
                table,
                alias,
                read_columns,
                predicates,
            } => format!(
                "Scan {} AS {} [rows={}, columns={}/{}, pushed_filters={}]",
                table.name,
                alias,
                table.stats.row_count,
                read_columns.len(),
                table.schema.len(),
                predicates.len()
            ),
            Self::Filter { predicate, .. } => {
                format!("Filter {predicate} [est_rows={}]", self.estimated_rows())
            }
            Self::Projection { expressions, .. } => format!(
                "Projection [{}]",
                expressions
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Aggregate {
                group_by,
                expressions,
                ..
            } => format!(
                "Aggregate group=[{}] output=[{}]",
                group_by
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                expressions
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Join { on, .. } => {
                format!("InnerJoin on={on} [est_rows={}]", self.estimated_rows())
            }
            Self::Sort { keys, .. } => format!(
                "Sort [{}]",
                keys.iter()
                    .map(|k| format!("{} {}", k.expr, if k.asc { "ASC" } else { "DESC" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Limit { count, .. } => format!("Limit {count}"),
        }
    }
}

pub fn estimate_scan_rows(table: &Table, predicates: &[ScalarExpr]) -> usize {
    if predicates.is_empty() {
        return table.stats.row_count;
    }
    let selectivity = predicates.iter().fold(1.0, |current, predicate| {
        current * predicate_selectivity(table, predicate)
    });
    let estimate = table.stats.row_count as f64 * selectivity.clamp(0.0, 1.0);
    if estimate == 0.0 {
        0
    } else {
        estimate.round().max(1.0) as usize
    }
}

fn predicate_selectivity(table: &Table, predicate: &ScalarExpr) -> f64 {
    let ScalarExpr::Binary {
        left, op, right, ..
    } = predicate
    else {
        return 0.1;
    };
    let (index, op, literal) = match (&**left, &**right) {
        (ScalarExpr::Column { index, .. }, ScalarExpr::Literal(value)) => (*index, *op, value),
        (ScalarExpr::Literal(value), ScalarExpr::Column { index, .. }) => {
            (*index, reverse_comparison(*op), value)
        }
        _ => return 0.1,
    };
    let Some(statistics) = table.stats.columns.get(index) else {
        return 0.1;
    };
    match op {
        BinaryOp::Eq => {
            if outside_range(literal, statistics.min.as_ref(), statistics.max.as_ref()) {
                0.0
            } else {
                1.0 / statistics.cardinality.max(1) as f64
            }
        }
        BinaryOp::NotEq => 1.0 - 1.0 / statistics.cardinality.max(1) as f64,
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => range_selectivity(
            statistics.min.as_ref(),
            statistics.max.as_ref(),
            literal,
            op,
        )
        .unwrap_or(0.33),
        _ => 0.1,
    }
}

fn outside_range(value: &Value, min: Option<&Value>, max: Option<&Value>) -> bool {
    min.is_some_and(|minimum| value < minimum) || max.is_some_and(|maximum| value > maximum)
}

fn range_selectivity(
    min: Option<&Value>,
    max: Option<&Value>,
    literal: &Value,
    op: BinaryOp,
) -> Option<f64> {
    let (minimum, maximum, value) = (numeric(min?)?, numeric(max?)?, numeric(literal)?);
    if maximum <= minimum {
        return Some(match op {
            BinaryOp::Lt | BinaryOp::LtEq => f64::from(value >= maximum),
            BinaryOp::Gt | BinaryOp::GtEq => f64::from(value <= minimum),
            _ => 0.1,
        });
    }
    let below = ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0);
    Some(match op {
        BinaryOp::Lt | BinaryOp::LtEq => below,
        BinaryOp::Gt | BinaryOp::GtEq => 1.0 - below,
        _ => 0.1,
    })
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Int64(value) => Some(*value as f64),
        Value::Float64(value) => Some(*value),
        _ => None,
    }
}

fn reverse_comparison(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::LtEq => BinaryOp::GtEq,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::GtEq => BinaryOp::LtEq,
        other => other,
    }
}

fn estimate_groups(plan: &LogicalPlan, expressions: &[ScalarExpr]) -> usize {
    let fallback = (plan.estimated_rows() / 10).max(1);
    expressions
        .iter()
        .try_fold(1usize, |groups, expression| {
            let ScalarExpr::Column { index, .. } = expression else {
                return None;
            };
            Some(groups.saturating_mul(plan.column_distinct(*index)?))
        })
        .unwrap_or(fallback)
        .min(plan.estimated_rows())
        .max(1)
}

fn estimate_join(left: &LogicalPlan, right: &LogicalPlan, on: &ScalarExpr) -> usize {
    let fallback = left.estimated_rows().max(right.estimated_rows());
    let ScalarExpr::Binary {
        left: first,
        op: BinaryOp::Eq,
        right: second,
        ..
    } = on
    else {
        return fallback;
    };
    let (ScalarExpr::Column { index: a, .. }, ScalarExpr::Column { index: b, .. }) =
        (&**first, &**second)
    else {
        return fallback;
    };
    let left_width = left.schema().len();
    let (left_index, right_index) = if *a < left_width && *b >= left_width {
        (*a, *b - left_width)
    } else if *b < left_width && *a >= left_width {
        (*b, *a - left_width)
    } else {
        return fallback;
    };
    let Some(divisor) = left
        .column_distinct(left_index)
        .zip(right.column_distinct(right_index))
        .map(|(a, b)| a.max(b).max(1))
    else {
        return fallback;
    };
    left.estimated_rows()
        .saturating_mul(right.estimated_rows())
        .div_ceil(divisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Field;

    #[test]
    fn range_estimate_uses_column_min_and_max() {
        let rows = (0..100).map(|value| vec![Value::Int64(value)]).collect();
        let table = Table::from_rows(
            "numbers",
            vec![Field {
                qualifier: None,
                name: "value".into(),
                data_type: DataType::Int64,
                nullable: false,
            }],
            rows,
        )
        .unwrap();
        let predicate = ScalarExpr::Binary {
            left: Box::new(ScalarExpr::Column {
                index: 0,
                qualifier: None,
                name: "value".into(),
                data_type: DataType::Int64,
            }),
            op: BinaryOp::GtEq,
            right: Box::new(ScalarExpr::Literal(Value::Int64(50))),
            data_type: DataType::Boolean,
        };
        assert_eq!(estimate_scan_rows(&table, &[predicate]), 49);
    }

    #[test]
    fn impossible_equality_estimates_zero_rows() {
        let table = Table::from_rows(
            "numbers",
            vec![Field {
                qualifier: None,
                name: "value".into(),
                data_type: DataType::Int64,
                nullable: false,
            }],
            vec![vec![Value::Int64(1)], vec![Value::Int64(2)]],
        )
        .unwrap();
        let predicate = ScalarExpr::Binary {
            left: Box::new(ScalarExpr::Column {
                index: 0,
                qualifier: None,
                name: "value".into(),
                data_type: DataType::Int64,
            }),
            op: BinaryOp::Eq,
            right: Box::new(ScalarExpr::Literal(Value::Int64(999))),
            data_type: DataType::Boolean,
        };
        assert_eq!(estimate_scan_rows(&table, &[predicate]), 0);
    }
}
