use crate::ast::{AggregateFunction, BinaryOp, Expr, Select, SelectItem, Statement, UnaryOp};
use crate::logical::{LogicalPlan, NamedExpr, ScalarExpr, SortExpr};
use crate::storage::Catalog;
use crate::types::{DataType, Schema};
use crate::{Error, Result};

pub struct Binder<'a> {
    catalog: &'a Catalog,
}

impl<'a> Binder<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    pub fn bind(&self, statement: &Statement) -> Result<LogicalPlan> {
        match statement {
            Statement::Select(select) => self.bind_select(select),
            Statement::Explain { statement, .. } => self.bind(statement),
        }
    }

    fn bind_select(&self, select: &Select) -> Result<LogicalPlan> {
        let table = self.catalog.table(&select.from.name).ok_or_else(|| {
            Error::Bind(format!(
                "unknown table {:?}; available tables: {}",
                select.from.name,
                self.catalog.table_names().join(", ")
            ))
        })?;
        let alias = select
            .from
            .alias
            .clone()
            .unwrap_or_else(|| select.from.name.clone());
        let mut plan = LogicalPlan::Scan {
            read_columns: (0..table.schema.len()).collect(),
            table,
            alias,
        };
        for join in &select.joins {
            let right_table = self
                .catalog
                .table(&join.table.name)
                .ok_or_else(|| Error::Bind(format!("unknown table {:?}", join.table.name)))?;
            let right_alias = join
                .table
                .alias
                .clone()
                .unwrap_or_else(|| join.table.name.clone());
            if plan.schema().iter().any(|field| {
                field
                    .qualifier
                    .as_deref()
                    .is_some_and(|q| q.eq_ignore_ascii_case(&right_alias))
            }) {
                return Err(Error::Bind(format!(
                    "duplicate table alias {right_alias:?}"
                )));
            }
            let right = LogicalPlan::Scan {
                read_columns: (0..right_table.schema.len()).collect(),
                table: right_table,
                alias: right_alias,
            };
            let mut join_schema = plan.schema();
            join_schema.extend(right.schema());
            let on = self.bind_expr(&join.on, &join_schema, false)?;
            require_boolean(&on, "JOIN ON")?;
            plan = LogicalPlan::Join {
                left: Box::new(plan),
                right: Box::new(right),
                on,
            };
        }
        if let Some(selection) = &select.selection {
            let predicate = self.bind_expr(selection, &plan.schema(), false)?;
            require_boolean(&predicate, "WHERE")?;
            plan = LogicalPlan::Filter {
                predicate,
                input: Box::new(plan),
            };
        }

        let input_schema = plan.schema();
        let group_by = select
            .group_by
            .iter()
            .map(|expr| self.bind_expr(expr, &input_schema, false))
            .collect::<Result<Vec<_>>>()?;
        let has_aggregate = select
            .projection
            .iter()
            .any(|item| matches!(item, SelectItem::Expr { expr, .. } if expr.contains_aggregate()));
        if has_aggregate || !group_by.is_empty() {
            let mut expressions = Vec::new();
            for item in &select.projection {
                let SelectItem::Expr { expr, alias } = item else {
                    return Err(Error::Bind(
                        "SELECT * is not valid with GROUP BY or aggregates".into(),
                    ));
                };
                let bound = self.bind_expr(expr, &input_schema, true)?;
                if bound.contains_aggregate()
                    && !matches!(bound, ScalarExpr::Aggregate { .. } | ScalarExpr::CountStar)
                {
                    return Err(Error::Bind("aggregate functions must be top-level SELECT expressions in this SQL subset".into()));
                }
                if !bound.contains_aggregate() && !group_by.contains(&bound) {
                    return Err(Error::Bind(format!(
                        "{expr} must appear in GROUP BY or be aggregated"
                    )));
                }
                expressions.push(NamedExpr {
                    name: alias.clone().unwrap_or_else(|| expr.to_string()),
                    expr: bound,
                });
            }
            plan = LogicalPlan::Aggregate {
                group_by,
                expressions,
                input: Box::new(plan),
            };
        } else {
            let mut expressions = Vec::new();
            for item in &select.projection {
                match item {
                    SelectItem::Wildcard => {
                        for (index, field) in input_schema.iter().enumerate() {
                            expressions.push(NamedExpr {
                                expr: ScalarExpr::Column {
                                    index,
                                    qualifier: field.qualifier.clone(),
                                    name: field.name.clone(),
                                    data_type: field.data_type,
                                },
                                name: field.name.clone(),
                            });
                        }
                    }
                    SelectItem::Expr { expr, alias } => {
                        let bound = self.bind_expr(expr, &input_schema, false)?;
                        expressions.push(NamedExpr {
                            name: alias.clone().unwrap_or_else(|| expr.to_string()),
                            expr: bound,
                        });
                    }
                }
            }
            plan = LogicalPlan::Projection {
                expressions,
                input: Box::new(plan),
            };
        }

        if !select.order_by.is_empty() {
            let output_schema = plan.schema();
            let keys = select
                .order_by
                .iter()
                .map(|key| {
                    Ok(SortExpr {
                        expr: self.bind_expr(&key.expr, &output_schema, false)?,
                        asc: key.asc,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            plan = LogicalPlan::Sort {
                keys,
                input: Box::new(plan),
            };
        }
        if let Some(count) = select.limit {
            plan = LogicalPlan::Limit {
                count,
                input: Box::new(plan),
            };
        }
        Ok(plan)
    }

    fn bind_expr(&self, expr: &Expr, schema: &Schema, allow_aggregate: bool) -> Result<ScalarExpr> {
        let _ = self.catalog;
        match expr {
            Expr::Literal(value) => Ok(ScalarExpr::Literal(value.clone())),
            Expr::Wildcard => Err(Error::Bind(
                "wildcard is only valid in SELECT * or COUNT(*)".into(),
            )),
            Expr::Column { qualifier, name } => {
                let matches = schema
                    .iter()
                    .enumerate()
                    .filter(|(_, field)| {
                        field.name.eq_ignore_ascii_case(name)
                            && qualifier.as_ref().is_none_or(|q| {
                                field
                                    .qualifier
                                    .as_ref()
                                    .is_some_and(|fq| fq.eq_ignore_ascii_case(q))
                            })
                    })
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [] => Err(Error::Bind(format!(
                        "unknown column {}{}",
                        qualifier
                            .as_ref()
                            .map(|q| format!("{q}."))
                            .unwrap_or_default(),
                        name
                    ))),
                    [(index, field)] => Ok(ScalarExpr::Column {
                        index: *index,
                        qualifier: field.qualifier.clone(),
                        name: field.name.clone(),
                        data_type: field.data_type,
                    }),
                    _ => Err(Error::Bind(format!(
                        "ambiguous column {name:?}; qualify it with a table alias"
                    ))),
                }
            }
            Expr::Unary { op, expr } => {
                let expr = self.bind_expr(expr, schema, allow_aggregate)?;
                let data_type = match op {
                    UnaryOp::Not if expr.data_type() == DataType::Boolean => DataType::Boolean,
                    UnaryOp::Neg if is_numeric(expr.data_type()) => expr.data_type(),
                    _ => {
                        return Err(Error::Bind(format!(
                            "operator {op:?} does not accept {}",
                            expr.data_type()
                        )));
                    }
                };
                Ok(ScalarExpr::Unary {
                    op: *op,
                    expr: Box::new(expr),
                    data_type,
                })
            }
            Expr::Binary { left, op, right } => {
                let left = self.bind_expr(left, schema, allow_aggregate)?;
                let right = self.bind_expr(right, schema, allow_aggregate)?;
                let data_type = binary_type(*op, left.data_type(), right.data_type())?;
                Ok(ScalarExpr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                    data_type,
                })
            }
            Expr::Aggregate { function, expr } => {
                if !allow_aggregate {
                    return Err(Error::Bind(
                        "aggregate functions are not allowed in WHERE, JOIN ON, or GROUP BY".into(),
                    ));
                }
                if **expr == Expr::Wildcard {
                    return if *function == AggregateFunction::Count {
                        Ok(ScalarExpr::CountStar)
                    } else {
                        Err(Error::Bind(format!(
                            "{function:?}(*) is not supported; only COUNT(*) is valid"
                        )))
                    };
                }
                let expr = self.bind_expr(expr, schema, false)?;
                let input_type = expr.data_type();
                let data_type = match function {
                    AggregateFunction::Count => DataType::Int64,
                    AggregateFunction::Avg if is_numeric(input_type) => DataType::Float64,
                    AggregateFunction::Sum if is_numeric(input_type) => input_type,
                    AggregateFunction::Min | AggregateFunction::Max
                        if input_type != DataType::Null =>
                    {
                        input_type
                    }
                    _ => {
                        return Err(Error::Bind(format!(
                            "{function:?} does not accept {input_type}"
                        )));
                    }
                };
                Ok(ScalarExpr::Aggregate {
                    function: *function,
                    expr: Box::new(expr),
                    data_type,
                })
            }
        }
    }
}

fn is_numeric(ty: DataType) -> bool {
    matches!(ty, DataType::Int64 | DataType::Float64)
}
fn compatible(left: DataType, right: DataType) -> bool {
    left == DataType::Null
        || right == DataType::Null
        || left == right
        || (is_numeric(left) && is_numeric(right))
}
fn binary_type(op: BinaryOp, left: DataType, right: DataType) -> Result<DataType> {
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
            if is_numeric(left) && is_numeric(right) =>
        {
            Ok(
                if left == DataType::Float64 || right == DataType::Float64 || op == BinaryOp::Div {
                    DataType::Float64
                } else {
                    DataType::Int64
                },
            )
        }
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Lt
        | BinaryOp::LtEq
        | BinaryOp::Gt
        | BinaryOp::GtEq
            if compatible(left, right) =>
        {
            Ok(DataType::Boolean)
        }
        BinaryOp::And | BinaryOp::Or if left == DataType::Boolean && right == DataType::Boolean => {
            Ok(DataType::Boolean)
        }
        _ => Err(Error::Bind(format!(
            "operator {op} does not accept {left} and {right}"
        ))),
    }
}
fn require_boolean(expr: &ScalarExpr, context: &str) -> Result<()> {
    if expr.data_type() == DataType::Boolean {
        Ok(())
    } else {
        Err(Error::Bind(format!(
            "{context} requires BOOLEAN, got {}",
            expr.data_type()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sql;
    use crate::storage::Table;
    use crate::types::{Field, Value};
    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.register(
            Table::from_rows(
                "a",
                vec![
                    Field {
                        qualifier: None,
                        name: "id".into(),
                        data_type: DataType::Int64,
                        nullable: false,
                    },
                    Field {
                        qualifier: None,
                        name: "name".into(),
                        data_type: DataType::Utf8,
                        nullable: false,
                    },
                ],
                vec![vec![Value::Int64(1), Value::Utf8("x".into())]],
            )
            .unwrap(),
        );
        c
    }
    fn bind(sql: &str) -> Result<LogicalPlan> {
        let c = catalog();
        Binder::new(&c).bind(&parse_sql(sql)?)
    }
    #[test]
    fn rejects_unknown_table() {
        assert!(
            bind("SELECT * FROM missing")
                .unwrap_err()
                .to_string()
                .contains("unknown table")
        );
    }
    #[test]
    fn rejects_unknown_column() {
        assert!(
            bind("SELECT nope FROM a")
                .unwrap_err()
                .to_string()
                .contains("unknown column")
        );
    }
    #[test]
    fn rejects_invalid_types_before_execution() {
        assert!(
            bind("SELECT name + 1 FROM a")
                .unwrap_err()
                .to_string()
                .contains("does not accept")
        );
    }
    #[test]
    fn binds_grouped_aggregate() {
        let p = bind("SELECT name, COUNT(*) AS n FROM a GROUP BY name").unwrap();
        assert!(matches!(p, LogicalPlan::Aggregate { .. }));
    }
    #[test]
    fn rejects_ungrouped_column() {
        assert!(bind("SELECT name, COUNT(*) FROM a").is_err());
    }
}
