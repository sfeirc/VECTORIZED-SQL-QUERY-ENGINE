use crate::ast::BinaryOp;
use crate::logical::{LogicalPlan, NamedExpr, ScalarExpr, SortExpr, estimate_scan_rows};
use crate::storage::Table;
use crate::types::{Field, Schema};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum JoinAlgorithm {
    Hash,
    NestedLoop,
}

#[derive(Debug, Clone, Copy)]
pub enum JoinPreference {
    Auto,
    Hash,
    NestedLoop,
}

#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    ColumnScan {
        table: Arc<Table>,
        alias: String,
        read_columns: Vec<usize>,
        predicates: Vec<ScalarExpr>,
    },
    Filter {
        predicate: ScalarExpr,
        input: Box<PhysicalPlan>,
    },
    Projection {
        expressions: Vec<NamedExpr>,
        input: Box<PhysicalPlan>,
    },
    HashAggregate {
        group_by: Vec<ScalarExpr>,
        expressions: Vec<NamedExpr>,
        input: Box<PhysicalPlan>,
    },
    Join {
        algorithm: JoinAlgorithm,
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        on: ScalarExpr,
    },
    Sort {
        keys: Vec<SortExpr>,
        input: Box<PhysicalPlan>,
    },
    Limit {
        count: usize,
        input: Box<PhysicalPlan>,
    },
}

pub fn create_physical_plan(plan: &LogicalPlan, preference: JoinPreference) -> PhysicalPlan {
    match plan {
        LogicalPlan::Scan {
            table,
            alias,
            read_columns,
            predicates,
        } => PhysicalPlan::ColumnScan {
            table: table.clone(),
            alias: alias.clone(),
            read_columns: read_columns.clone(),
            predicates: predicates.clone(),
        },
        LogicalPlan::Filter { predicate, input } => PhysicalPlan::Filter {
            predicate: predicate.clone(),
            input: Box::new(create_physical_plan(input, preference)),
        },
        LogicalPlan::Projection { expressions, input } => PhysicalPlan::Projection {
            expressions: expressions.clone(),
            input: Box::new(create_physical_plan(input, preference)),
        },
        LogicalPlan::Aggregate {
            group_by,
            expressions,
            input,
        } => PhysicalPlan::HashAggregate {
            group_by: group_by.clone(),
            expressions: expressions.clone(),
            input: Box::new(create_physical_plan(input, preference)),
        },
        LogicalPlan::Join { left, right, on } => {
            let equi_join = matches!(on, ScalarExpr::Binary { op: crate::ast::BinaryOp::Eq, left, right, .. }
                if matches!(**left, ScalarExpr::Column { .. }) && matches!(**right, ScalarExpr::Column { .. }));
            let product = left.estimated_rows().saturating_mul(right.estimated_rows());
            let algorithm = match preference {
                JoinPreference::Hash if equi_join => JoinAlgorithm::Hash,
                JoinPreference::Hash | JoinPreference::NestedLoop => JoinAlgorithm::NestedLoop,
                JoinPreference::Auto if equi_join && product >= 64 => JoinAlgorithm::Hash,
                JoinPreference::Auto => JoinAlgorithm::NestedLoop,
            };
            PhysicalPlan::Join {
                algorithm,
                left: Box::new(create_physical_plan(left, preference)),
                right: Box::new(create_physical_plan(right, preference)),
                on: on.clone(),
            }
        }
        LogicalPlan::Sort { keys, input } => PhysicalPlan::Sort {
            keys: keys.clone(),
            input: Box::new(create_physical_plan(input, preference)),
        },
        LogicalPlan::Limit { count, input } => PhysicalPlan::Limit {
            count: *count,
            input: Box::new(create_physical_plan(input, preference)),
        },
    }
}

impl PhysicalPlan {
    pub fn schema(&self) -> Schema {
        match self {
            Self::ColumnScan { table, alias, .. } => table
                .schema
                .iter()
                .map(|field| Field {
                    qualifier: Some(alias.clone()),
                    name: field.name.clone(),
                    data_type: field.data_type,
                    nullable: field.nullable,
                })
                .collect(),
            Self::Filter { input, .. } | Self::Sort { input, .. } | Self::Limit { input, .. } => {
                input.schema()
            }
            Self::Projection { expressions, .. } | Self::HashAggregate { expressions, .. } => {
                expressions
                    .iter()
                    .map(|expression| Field {
                        qualifier: None,
                        name: expression.name.clone(),
                        data_type: expression.expr.data_type(),
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
            Self::ColumnScan {
                table, predicates, ..
            } => estimate_scan_rows(table, predicates),
            Self::Filter { input, .. } => (input.estimated_rows() / 10).max(1),
            Self::Projection { input, .. } | Self::Sort { input, .. } => input.estimated_rows(),
            Self::HashAggregate {
                group_by, input, ..
            } => {
                if group_by.is_empty() {
                    1
                } else {
                    estimate_groups(input, group_by)
                }
            }
            Self::Join {
                left, right, on, ..
            } => estimate_join(left, right, on),
            Self::Limit { count, input } => (*count).min(input.estimated_rows()),
        }
    }

    pub fn column_distinct(&self, index: usize) -> Option<usize> {
        match self {
            Self::ColumnScan { table, .. } => table
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
            | Self::HashAggregate {
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
        let next = format!("{prefix}{}", if last { "    " } else { "│   " });
        match self {
            Self::ColumnScan { .. } => {}
            Self::Join { left, right, .. } => {
                left.format_node(output, &next, false);
                right.format_node(output, &next, true);
            }
            Self::Filter { input, .. }
            | Self::Projection { input, .. }
            | Self::HashAggregate { input, .. }
            | Self::Sort { input, .. }
            | Self::Limit { input, .. } => input.format_node(output, &next, true),
        }
    }
    fn label(&self) -> String {
        match self {
            Self::ColumnScan {
                table,
                read_columns,
                predicates,
                ..
            } => format!(
                "ColumnScan {} [columns={}/{}, pushed_filters={}, est_rows={}]",
                table.name,
                read_columns.len(),
                table.schema.len(),
                predicates.len(),
                self.estimated_rows()
            ),
            Self::Filter { predicate, .. } => format!(
                "VectorFilter {predicate} [est_rows={}]",
                self.estimated_rows()
            ),
            Self::Projection { expressions, .. } => format!(
                "VectorProjection [{}]",
                expressions
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::HashAggregate { group_by, .. } => format!(
                "HashAggregate [groups={}, est_rows={}]",
                group_by.len(),
                self.estimated_rows()
            ),
            Self::Join { algorithm, on, .. } => format!(
                "{algorithm:?}Join on={on} [est_rows={}]",
                self.estimated_rows()
            ),
            Self::Sort { keys, .. } => format!(
                "Sort [{}]",
                keys.iter()
                    .map(|key| key.expr.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Limit { count, .. } => format!("Limit {count}"),
        }
    }
}

fn estimate_groups(plan: &PhysicalPlan, expressions: &[ScalarExpr]) -> usize {
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

fn estimate_join(left: &PhysicalPlan, right: &PhysicalPlan, on: &ScalarExpr) -> usize {
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
