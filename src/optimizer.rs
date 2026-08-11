use crate::ast::{BinaryOp, UnaryOp};
use crate::logical::{LogicalPlan, NamedExpr, ScalarExpr};
use crate::types::Value;
use crate::{Error, Result};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
pub struct OptimizerOptions {
    pub predicate_pushdown: bool,
    pub projection_pruning: bool,
    pub constant_folding: bool,
    pub filter_simplification: bool,
    pub join_order: bool,
}

impl Default for OptimizerOptions {
    fn default() -> Self {
        Self {
            predicate_pushdown: true,
            projection_pruning: true,
            constant_folding: true,
            filter_simplification: true,
            join_order: true,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct OptimizationReport {
    pub constants_folded: usize,
    pub filters_simplified: usize,
    pub predicates_pushed: usize,
    pub columns_pruned: usize,
    pub joins_reordered: usize,
}

pub fn optimize(
    mut plan: LogicalPlan,
    options: OptimizerOptions,
) -> Result<(LogicalPlan, OptimizationReport)> {
    let mut report = OptimizationReport::default();
    if options.constant_folding {
        plan = fold_plan(plan, &mut report)?;
    }
    if options.filter_simplification {
        plan = simplify_filters(plan, &mut report);
    }
    if options.predicate_pushdown {
        plan = push_predicates(plan, &mut report);
    }
    if options.join_order {
        plan = reorder_joins(plan, &mut report);
    }
    if options.projection_pruning {
        let needed = (0..plan.schema().len()).collect();
        prune_columns(&mut plan, needed, &mut report);
    }
    Ok((plan, report))
}

fn map_children(
    plan: LogicalPlan,
    f: &mut impl FnMut(LogicalPlan) -> Result<LogicalPlan>,
) -> Result<LogicalPlan> {
    Ok(match plan {
        LogicalPlan::Filter { predicate, input } => LogicalPlan::Filter {
            predicate,
            input: Box::new(f(*input)?),
        },
        LogicalPlan::Projection { expressions, input } => LogicalPlan::Projection {
            expressions,
            input: Box::new(f(*input)?),
        },
        LogicalPlan::Aggregate {
            group_by,
            expressions,
            input,
        } => LogicalPlan::Aggregate {
            group_by,
            expressions,
            input: Box::new(f(*input)?),
        },
        LogicalPlan::Join { left, right, on } => LogicalPlan::Join {
            left: Box::new(f(*left)?),
            right: Box::new(f(*right)?),
            on,
        },
        LogicalPlan::Sort { keys, input } => LogicalPlan::Sort {
            keys,
            input: Box::new(f(*input)?),
        },
        LogicalPlan::Limit { count, input } => LogicalPlan::Limit {
            count,
            input: Box::new(f(*input)?),
        },
        scan @ LogicalPlan::Scan { .. } => scan,
    })
}

fn fold_plan(plan: LogicalPlan, report: &mut OptimizationReport) -> Result<LogicalPlan> {
    let plan = map_children(plan, &mut |child| fold_plan(child, report))?;
    Ok(match plan {
        LogicalPlan::Filter { predicate, input } => LogicalPlan::Filter {
            predicate: fold_expr(predicate, report)?,
            input,
        },
        LogicalPlan::Projection { expressions, input } => LogicalPlan::Projection {
            expressions: fold_named(expressions, report)?,
            input,
        },
        LogicalPlan::Aggregate {
            group_by,
            expressions,
            input,
        } => LogicalPlan::Aggregate {
            group_by: group_by
                .into_iter()
                .map(|e| fold_expr(e, report))
                .collect::<Result<_>>()?,
            expressions: fold_named(expressions, report)?,
            input,
        },
        LogicalPlan::Join { left, right, on } => LogicalPlan::Join {
            left,
            right,
            on: fold_expr(on, report)?,
        },
        LogicalPlan::Sort { keys, input } => LogicalPlan::Sort {
            keys: keys
                .into_iter()
                .map(|mut key| {
                    key.expr = fold_expr(key.expr, report)?;
                    Ok(key)
                })
                .collect::<Result<_>>()?,
            input,
        },
        other => other,
    })
}

fn fold_named(
    expressions: Vec<NamedExpr>,
    report: &mut OptimizationReport,
) -> Result<Vec<NamedExpr>> {
    expressions
        .into_iter()
        .map(|mut expression| {
            expression.expr = fold_expr(expression.expr, report)?;
            Ok(expression)
        })
        .collect()
}

fn fold_expr(expr: ScalarExpr, report: &mut OptimizationReport) -> Result<ScalarExpr> {
    let rebuilt = match expr {
        ScalarExpr::Unary {
            op,
            expr,
            data_type,
        } => ScalarExpr::Unary {
            op,
            expr: Box::new(fold_expr(*expr, report)?),
            data_type,
        },
        ScalarExpr::Binary {
            left,
            op,
            right,
            data_type,
        } => ScalarExpr::Binary {
            left: Box::new(fold_expr(*left, report)?),
            op,
            right: Box::new(fold_expr(*right, report)?),
            data_type,
        },
        ScalarExpr::Aggregate {
            function,
            expr,
            data_type,
        } => ScalarExpr::Aggregate {
            function,
            expr: Box::new(fold_expr(*expr, report)?),
            data_type,
        },
        other => other,
    };
    match &rebuilt {
        ScalarExpr::Unary { op, expr, .. } if matches!(**expr, ScalarExpr::Literal(_)) => {
            let ScalarExpr::Literal(value) = &**expr else {
                unreachable!()
            };
            report.constants_folded += 1;
            Ok(ScalarExpr::Literal(eval_unary(*op, value.clone())?))
        }
        ScalarExpr::Binary {
            left, op, right, ..
        } if matches!(**left, ScalarExpr::Literal(_))
            && matches!(**right, ScalarExpr::Literal(_)) =>
        {
            let (ScalarExpr::Literal(a), ScalarExpr::Literal(b)) = (&**left, &**right) else {
                unreachable!()
            };
            report.constants_folded += 1;
            Ok(ScalarExpr::Literal(eval_binary(a.clone(), *op, b.clone())?))
        }
        _ => Ok(rebuilt),
    }
}

fn simplify_filters(plan: LogicalPlan, report: &mut OptimizationReport) -> LogicalPlan {
    let plan = map_children(plan, &mut |child| Ok(simplify_filters(child, report)))
        .expect("recursive simplification cannot fail");
    match plan {
        LogicalPlan::Filter {
            predicate: ScalarExpr::Literal(Value::Boolean(true)),
            input,
        } => {
            report.filters_simplified += 1;
            *input
        }
        LogicalPlan::Filter {
            predicate:
                ScalarExpr::Binary {
                    left,
                    op: BinaryOp::And,
                    right,
                    data_type,
                },
            input,
        } => match (&*left, &*right) {
            (ScalarExpr::Literal(Value::Boolean(true)), _) => {
                report.filters_simplified += 1;
                LogicalPlan::Filter {
                    predicate: *right,
                    input,
                }
            }
            (_, ScalarExpr::Literal(Value::Boolean(true))) => {
                report.filters_simplified += 1;
                LogicalPlan::Filter {
                    predicate: *left,
                    input,
                }
            }
            _ => LogicalPlan::Filter {
                predicate: ScalarExpr::Binary {
                    left,
                    op: BinaryOp::And,
                    right,
                    data_type,
                },
                input,
            },
        },
        other => other,
    }
}

fn push_predicates(plan: LogicalPlan, report: &mut OptimizationReport) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter { predicate, input } => {
            let input = push_predicates(*input, report);
            match input {
                LogicalPlan::Scan {
                    table,
                    alias,
                    read_columns,
                    mut predicates,
                } => {
                    let pushed = split_conjuncts(predicate);
                    report.predicates_pushed += pushed.len();
                    predicates.extend(pushed);
                    LogicalPlan::Scan {
                        table,
                        alias,
                        read_columns,
                        predicates,
                    }
                }
                LogicalPlan::Join { left, right, on } => {
                    let left_width = left.schema().len();
                    let total = left_width + right.schema().len();
                    let mut left_predicates = Vec::new();
                    let mut right_predicates = Vec::new();
                    let mut remaining = Vec::new();
                    for conjunct in split_conjuncts(predicate) {
                        let mut indices = Vec::new();
                        conjunct.column_indices(&mut indices);
                        if indices.iter().all(|i| *i < left_width) {
                            left_predicates.push(conjunct);
                        } else if indices.iter().all(|i| *i >= left_width && *i < total) {
                            right_predicates.push(remap_expr(conjunct, &|i| i - left_width));
                        } else {
                            remaining.push(conjunct);
                        }
                    }
                    let left = left_predicates.into_iter().fold(*left, |plan, predicate| {
                        push_predicates(
                            LogicalPlan::Filter {
                                predicate,
                                input: Box::new(plan),
                            },
                            report,
                        )
                    });
                    let right = right_predicates
                        .into_iter()
                        .fold(*right, |plan, predicate| {
                            push_predicates(
                                LogicalPlan::Filter {
                                    predicate,
                                    input: Box::new(plan),
                                },
                                report,
                            )
                        });
                    let joined = LogicalPlan::Join {
                        left: Box::new(left),
                        right: Box::new(right),
                        on,
                    };
                    remaining
                        .into_iter()
                        .fold(joined, |plan, predicate| LogicalPlan::Filter {
                            predicate,
                            input: Box::new(plan),
                        })
                }
                other => LogicalPlan::Filter {
                    predicate,
                    input: Box::new(other),
                },
            }
        }
        other => map_children(other, &mut |child| Ok(push_predicates(child, report)))
            .expect("recursive pushdown cannot fail"),
    }
}

fn split_conjuncts(expr: ScalarExpr) -> Vec<ScalarExpr> {
    match expr {
        ScalarExpr::Binary {
            left,
            op: BinaryOp::And,
            right,
            ..
        } => {
            let mut output = split_conjuncts(*left);
            output.extend(split_conjuncts(*right));
            output
        }
        other => vec![other],
    }
}

fn reorder_joins(plan: LogicalPlan, report: &mut OptimizationReport) -> LogicalPlan {
    let plan = map_children(plan, &mut |child| Ok(reorder_joins(child, report)))
        .expect("recursive join ordering cannot fail");
    let LogicalPlan::Join { left, right, on } = plan else {
        return plan;
    };
    if right.estimated_rows() >= left.estimated_rows() {
        return LogicalPlan::Join { left, right, on };
    }
    let left_schema = left.schema();
    let right_schema = right.schema();
    let left_width = left_schema.len();
    let right_width = right_schema.len();
    let swapped_on = remap_expr(on, &|index| {
        if index < left_width {
            right_width + index
        } else {
            index - left_width
        }
    });
    let swapped = LogicalPlan::Join {
        left: right,
        right: left,
        on: swapped_on,
    };
    let expressions = left_schema
        .iter()
        .enumerate()
        .map(|(index, field)| NamedExpr {
            expr: ScalarExpr::Column {
                index: right_width + index,
                qualifier: field.qualifier.clone(),
                name: field.name.clone(),
                data_type: field.data_type,
            },
            name: field.name.clone(),
        })
        .chain(
            right_schema
                .iter()
                .enumerate()
                .map(|(index, field)| NamedExpr {
                    expr: ScalarExpr::Column {
                        index,
                        qualifier: field.qualifier.clone(),
                        name: field.name.clone(),
                        data_type: field.data_type,
                    },
                    name: field.name.clone(),
                }),
        )
        .collect();
    report.joins_reordered += 1;
    LogicalPlan::Projection {
        expressions,
        input: Box::new(swapped),
    }
}

fn remap_expr(expr: ScalarExpr, map: &impl Fn(usize) -> usize) -> ScalarExpr {
    match expr {
        ScalarExpr::Column {
            index,
            qualifier,
            name,
            data_type,
        } => ScalarExpr::Column {
            index: map(index),
            qualifier,
            name,
            data_type,
        },
        ScalarExpr::Unary {
            op,
            expr,
            data_type,
        } => ScalarExpr::Unary {
            op,
            expr: Box::new(remap_expr(*expr, map)),
            data_type,
        },
        ScalarExpr::Binary {
            left,
            op,
            right,
            data_type,
        } => ScalarExpr::Binary {
            left: Box::new(remap_expr(*left, map)),
            op,
            right: Box::new(remap_expr(*right, map)),
            data_type,
        },
        ScalarExpr::Aggregate {
            function,
            expr,
            data_type,
        } => ScalarExpr::Aggregate {
            function,
            expr: Box::new(remap_expr(*expr, map)),
            data_type,
        },
        other => other,
    }
}

fn prune_columns(plan: &mut LogicalPlan, needed: BTreeSet<usize>, report: &mut OptimizationReport) {
    match plan {
        LogicalPlan::Scan {
            table,
            read_columns,
            predicates,
            ..
        } => {
            let mut required = needed;
            for predicate in predicates {
                let mut columns = Vec::new();
                predicate.column_indices(&mut columns);
                required.extend(columns);
            }
            let old = read_columns.len();
            *read_columns = required.into_iter().collect();
            report.columns_pruned += old.saturating_sub(read_columns.len());
            debug_assert!(read_columns.iter().all(|i| *i < table.schema.len()));
        }
        LogicalPlan::Filter { predicate, input } => {
            let mut required = needed;
            let mut columns = Vec::new();
            predicate.column_indices(&mut columns);
            required.extend(columns);
            prune_columns(input, required, report);
        }
        LogicalPlan::Projection { expressions, input } => {
            let mut required = BTreeSet::new();
            for index in needed {
                if let Some(expression) = expressions.get(index) {
                    let mut columns = Vec::new();
                    expression.expr.column_indices(&mut columns);
                    required.extend(columns);
                }
            }
            prune_columns(input, required, report);
        }
        LogicalPlan::Aggregate {
            group_by,
            expressions,
            input,
        } => {
            let mut required = BTreeSet::new();
            for expression in group_by.iter() {
                let mut columns = Vec::new();
                expression.column_indices(&mut columns);
                required.extend(columns);
            }
            for index in needed {
                if let Some(expression) = expressions.get(index) {
                    let mut columns = Vec::new();
                    expression.expr.column_indices(&mut columns);
                    required.extend(columns);
                }
            }
            prune_columns(input, required, report);
        }
        LogicalPlan::Join { left, right, on } => {
            let left_width = left.schema().len();
            let mut on_columns = Vec::new();
            on.column_indices(&mut on_columns);
            let mut left_needed = BTreeSet::new();
            let mut right_needed = BTreeSet::new();
            for index in needed.into_iter().chain(on_columns) {
                if index < left_width {
                    left_needed.insert(index);
                } else {
                    right_needed.insert(index - left_width);
                }
            }
            prune_columns(left, left_needed, report);
            prune_columns(right, right_needed, report);
        }
        LogicalPlan::Sort { keys, input } => {
            let mut required = needed;
            for key in keys {
                let mut columns = Vec::new();
                key.expr.column_indices(&mut columns);
                required.extend(columns);
            }
            prune_columns(input, required, report);
        }
        LogicalPlan::Limit { input, .. } => prune_columns(input, needed, report),
    }
}

pub fn eval_unary(op: UnaryOp, value: Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match (op, value) {
        (UnaryOp::Not, Value::Boolean(value)) => Ok(Value::Boolean(!value)),
        (UnaryOp::Neg, Value::Int64(value)) => Ok(Value::Int64(-value)),
        (UnaryOp::Neg, Value::Float64(value)) => Ok(Value::Float64(-value)),
        _ => Err(Error::Execution(
            "invalid unary expression reached executor".into(),
        )),
    }
}

pub fn eval_binary(left: Value, op: BinaryOp, right: Value) -> Result<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    let numeric = |a: f64, b: f64| -> Result<Value> {
        Ok(match op {
            BinaryOp::Add => Value::Float64(a + b),
            BinaryOp::Sub => Value::Float64(a - b),
            BinaryOp::Mul => Value::Float64(a * b),
            BinaryOp::Div if b != 0.0 => Value::Float64(a / b),
            BinaryOp::Div => return Err(Error::Execution("division by zero".into())),
            _ => unreachable!(),
        })
    };
    match (&left, &right) {
        (Value::Int64(a), Value::Int64(b))
            if matches!(op, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul) =>
        {
            return Ok(Value::Int64(match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                _ => a * b,
            }));
        }
        (Value::Int64(a), Value::Int64(b)) if op == BinaryOp::Div => {
            return numeric(*a as f64, *b as f64);
        }
        (Value::Int64(a), Value::Float64(b))
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
            ) =>
        {
            return numeric(*a as f64, *b);
        }
        (Value::Float64(a), Value::Int64(b))
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
            ) =>
        {
            return numeric(*a, *b as f64);
        }
        (Value::Float64(a), Value::Float64(b))
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
            ) =>
        {
            return numeric(*a, *b);
        }
        _ => {}
    }
    Ok(match op {
        BinaryOp::Eq => Value::Boolean(left == right),
        BinaryOp::NotEq => Value::Boolean(left != right),
        BinaryOp::Lt => Value::Boolean(left < right),
        BinaryOp::LtEq => Value::Boolean(left <= right),
        BinaryOp::Gt => Value::Boolean(left > right),
        BinaryOp::GtEq => Value::Boolean(left >= right),
        BinaryOp::And => Value::Boolean(left.as_bool().unwrap() && right.as_bool().unwrap()),
        BinaryOp::Or => Value::Boolean(left.as_bool().unwrap() || right.as_bool().unwrap()),
        _ => {
            return Err(Error::Execution(
                "invalid binary expression reached executor".into(),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binder::Binder;
    use crate::parse_sql;
    use crate::storage::{Catalog, Table};
    use crate::types::{DataType, Field};

    fn plan(sql: &str) -> LogicalPlan {
        let mut catalog = Catalog::default();
        catalog.register(
            Table::from_rows(
                "t",
                vec![
                    Field {
                        qualifier: None,
                        name: "a".into(),
                        data_type: DataType::Int64,
                        nullable: false,
                    },
                    Field {
                        qualifier: None,
                        name: "b".into(),
                        data_type: DataType::Utf8,
                        nullable: false,
                    },
                ],
                vec![vec![Value::Int64(1), Value::Utf8("x".into())]],
            )
            .unwrap(),
        );
        Binder::new(&catalog)
            .bind(&parse_sql(sql).unwrap())
            .unwrap()
    }

    #[test]
    fn folds_and_simplifies_constants() {
        let (plan, report) = optimize(
            plan("SELECT a FROM t WHERE 1 + 1 = 2 AND true"),
            OptimizerOptions::default(),
        )
        .unwrap();
        assert!(report.constants_folded >= 2);
        assert!(report.filters_simplified >= 1);
        assert!(!plan.format_tree().contains("Filter"));
    }

    #[test]
    fn pushes_predicate_and_prunes_projection() {
        let (plan, report) = optimize(
            plan("SELECT a FROM t WHERE a > 0"),
            OptimizerOptions::default(),
        )
        .unwrap();
        assert_eq!(report.predicates_pushed, 1);
        assert_eq!(report.columns_pruned, 1);
        assert!(plan.format_tree().contains("pushed_filters=1"));
    }
}
