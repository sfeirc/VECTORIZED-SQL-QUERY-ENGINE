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
            Self::Scan { table, .. } => table.stats.row_count,
            Self::Filter { input, .. } => (input.estimated_rows() / 10).max(1),
            Self::Projection { input, .. } | Self::Sort { input, .. } => input.estimated_rows(),
            Self::Aggregate {
                group_by, input, ..
            } => {
                if group_by.is_empty() {
                    1
                } else {
                    (input.estimated_rows() / 10).max(1)
                }
            }
            Self::Join { left, right, .. } => left.estimated_rows().max(right.estimated_rows()),
            Self::Limit { count, input } => (*count).min(input.estimated_rows()),
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
