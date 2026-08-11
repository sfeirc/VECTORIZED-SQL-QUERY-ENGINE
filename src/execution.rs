use crate::ast::AggregateFunction;
use crate::logical::{NamedExpr, ScalarExpr, SortExpr};
use crate::optimizer::{eval_binary, eval_unary};
use crate::physical::{JoinAlgorithm, PhysicalPlan};
use crate::types::{DataType, Schema, Value};
use crate::{Error, Result};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExecutionMode {
    Vectorized,
    Tuple,
}

#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub batch_size: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Vectorized,
            batch_size: 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DataSet {
    pub schema: Schema,
    pub columns: Vec<Vec<Value>>,
    pub row_count: usize,
}

impl DataSet {
    pub fn row(&self, index: usize) -> Vec<Value> {
        self.columns
            .iter()
            .map(|column| column.get(index).cloned().unwrap_or(Value::Null))
            .collect()
    }
    pub fn rows(&self) -> Vec<Vec<Value>> {
        (0..self.row_count).map(|index| self.row(index)).collect()
    }
    pub fn estimated_bytes(&self) -> usize {
        self.columns.iter().flatten().map(value_bytes).sum()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorProfile {
    pub operator: String,
    pub estimated_rows: usize,
    pub rows_in: usize,
    pub rows_out: usize,
    pub elapsed_ns: u128,
    pub memory_bytes: usize,
    pub children: Vec<OperatorProfile>,
}

impl OperatorProfile {
    pub fn format_tree(&self) -> String {
        let mut output = String::new();
        self.format_node(&mut output, "", true, self.elapsed_ns.max(1));
        output
    }
    fn format_node(&self, output: &mut String, prefix: &str, last: bool, total: u128) {
        output.push_str(prefix);
        output.push_str(if last { "└── " } else { "├── " });
        let percent = self.elapsed_ns as f64 / total as f64 * 100.0;
        output.push_str(&format!(
            "{} [est={}, in={}, out={}, {:.3} ms, {:.1}%, {} bytes]\n",
            self.operator,
            self.estimated_rows,
            self.rows_in,
            self.rows_out,
            self.elapsed_ns as f64 / 1_000_000.0,
            percent,
            self.memory_bytes
        ));
        let next = format!("{prefix}{}", if last { "    " } else { "│   " });
        for (index, child) in self.children.iter().enumerate() {
            child.format_node(output, &next, index + 1 == self.children.len(), total);
        }
    }
}

pub struct ExecutionResult {
    pub data: DataSet,
    pub profile: OperatorProfile,
}

pub fn execute(plan: &PhysicalPlan, config: &ExecutionConfig) -> Result<ExecutionResult> {
    if config.batch_size == 0 {
        return Err(Error::Execution(
            "batch size must be greater than zero".into(),
        ));
    }
    execute_node(plan, config)
}

fn execute_node(plan: &PhysicalPlan, config: &ExecutionConfig) -> Result<ExecutionResult> {
    let started = Instant::now();
    let (data, rows_in, children, operator) = match plan {
        PhysicalPlan::ColumnScan {
            table,
            alias,
            read_columns,
            predicates,
        } => {
            let schema = plan.schema();
            let mut columns = vec![Vec::new(); schema.len()];
            let chunk = effective_batch(config);
            for start in (0..table.stats.row_count).step_by(chunk) {
                let end = (start + chunk).min(table.stats.row_count);
                let selected = (start..end)
                    .filter(|row| {
                        predicates.iter().all(|predicate| {
                            eval_expr(predicate, &|index| Ok(table.columns[index].value(*row)))
                                .ok()
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        })
                    })
                    .collect::<Vec<_>>();
                for &index in read_columns {
                    columns[index]
                        .extend(selected.iter().map(|row| table.columns[index].value(*row)));
                }
            }
            let row_count = if predicates.is_empty() {
                table.stats.row_count
            } else {
                columns
                    .iter()
                    .find(|c| !c.is_empty())
                    .map_or_else(|| count_scan_matches(table, predicates), Vec::len)
            };
            let _ = alias;
            (
                DataSet {
                    schema,
                    columns,
                    row_count,
                },
                table.stats.row_count,
                Vec::new(),
                "ColumnScan",
            )
        }
        PhysicalPlan::Filter { predicate, input } => {
            let child = execute_node(input, config)?;
            let rows_in = child.data.row_count;
            let selected = selected_rows(&child.data, predicate, effective_batch(config))?;
            let data = take_rows(&child.data, &selected);
            (data, rows_in, vec![child.profile], "VectorFilter")
        }
        PhysicalPlan::Projection { expressions, input } => {
            let child = execute_node(input, config)?;
            let rows_in = child.data.row_count;
            let data = project(&child.data, expressions, effective_batch(config))?;
            (data, rows_in, vec![child.profile], "VectorProjection")
        }
        PhysicalPlan::HashAggregate {
            group_by,
            expressions,
            input,
        } => {
            let child = execute_node(input, config)?;
            let rows_in = child.data.row_count;
            let data = aggregate(&child.data, group_by, expressions, effective_batch(config))?;
            (data, rows_in, vec![child.profile], "HashAggregate")
        }
        PhysicalPlan::Join {
            algorithm,
            left,
            right,
            on,
        } => {
            let left_result = execute_node(left, config)?;
            let right_result = execute_node(right, config)?;
            let rows_in = left_result.data.row_count + right_result.data.row_count;
            let data = match algorithm {
                JoinAlgorithm::Hash => hash_join(&left_result.data, &right_result.data, on)?,
                JoinAlgorithm::NestedLoop => {
                    nested_loop_join(&left_result.data, &right_result.data, on)?
                }
            };
            let name = if *algorithm == JoinAlgorithm::Hash {
                "HashJoin"
            } else {
                "NestedLoopJoin"
            };
            (
                data,
                rows_in,
                vec![left_result.profile, right_result.profile],
                name,
            )
        }
        PhysicalPlan::Sort { keys, input } => {
            let child = execute_node(input, config)?;
            let rows_in = child.data.row_count;
            let data = sort(&child.data, keys)?;
            (data, rows_in, vec![child.profile], "Sort")
        }
        PhysicalPlan::Limit { count, input } => {
            let child = execute_node(input, config)?;
            let rows_in = child.data.row_count;
            let selected = (0..(*count).min(rows_in)).collect::<Vec<_>>();
            let data = take_rows(&child.data, &selected);
            (data, rows_in, vec![child.profile], "Limit")
        }
    };
    let profile = OperatorProfile {
        operator: operator.into(),
        estimated_rows: plan.estimated_rows(),
        rows_in,
        rows_out: data.row_count,
        elapsed_ns: started.elapsed().as_nanos(),
        memory_bytes: data.estimated_bytes(),
        children,
    };
    Ok(ExecutionResult { data, profile })
}

fn effective_batch(config: &ExecutionConfig) -> usize {
    if config.mode == ExecutionMode::Tuple {
        1
    } else {
        config.batch_size
    }
}

fn selected_rows(data: &DataSet, predicate: &ScalarExpr, batch: usize) -> Result<Vec<usize>> {
    let mut selected = Vec::new();
    for start in (0..data.row_count).step_by(batch) {
        let end = (start + batch).min(data.row_count);
        for row in start..end {
            if eval_expr(predicate, &|index| data_value(data, index, row))?.as_bool() == Some(true)
            {
                selected.push(row);
            }
        }
    }
    Ok(selected)
}

fn project(data: &DataSet, expressions: &[NamedExpr], batch: usize) -> Result<DataSet> {
    let mut columns = vec![Vec::with_capacity(data.row_count); expressions.len()];
    for start in (0..data.row_count).step_by(batch) {
        let end = (start + batch).min(data.row_count);
        for (column, expression) in columns.iter_mut().zip(expressions) {
            for row in start..end {
                column.push(eval_expr(&expression.expr, &|index| {
                    data_value(data, index, row)
                })?);
            }
        }
    }
    let schema = expressions
        .iter()
        .map(|expression| crate::types::Field {
            qualifier: None,
            name: expression.name.clone(),
            data_type: expression.expr.data_type(),
            nullable: true,
        })
        .collect();
    Ok(DataSet {
        schema,
        columns,
        row_count: data.row_count,
    })
}

fn aggregate(
    data: &DataSet,
    group_by: &[ScalarExpr],
    expressions: &[NamedExpr],
    batch: usize,
) -> Result<DataSet> {
    let mut groups: HashMap<Vec<Value>, Vec<AggState>> = HashMap::new();
    if data.row_count == 0 && group_by.is_empty() {
        groups.insert(Vec::new(), initial_states(expressions));
    }
    for start in (0..data.row_count).step_by(batch) {
        let end = (start + batch).min(data.row_count);
        for row in start..end {
            let key = group_by
                .iter()
                .map(|expr| eval_expr(expr, &|index| data_value(data, index, row)))
                .collect::<Result<Vec<_>>>()?;
            let states = groups
                .entry(key)
                .or_insert_with(|| initial_states(expressions));
            for (state, expression) in states.iter_mut().zip(expressions) {
                state.update(&expression.expr, data, row)?;
            }
        }
    }
    let mut entries = groups.into_iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
    let mut columns = vec![Vec::with_capacity(entries.len()); expressions.len()];
    for (_, states) in entries {
        for (column, state) in columns.iter_mut().zip(states) {
            column.push(state.finish());
        }
    }
    let row_count = columns.first().map_or(0, Vec::len);
    let schema = expressions
        .iter()
        .map(|expression| crate::types::Field {
            qualifier: None,
            name: expression.name.clone(),
            data_type: expression.expr.data_type(),
            nullable: true,
        })
        .collect();
    Ok(DataSet {
        schema,
        columns,
        row_count,
    })
}

enum AggState {
    Value(Option<Value>),
    Count(i64),
    SumInt(i64, bool),
    SumFloat(f64, bool),
    Avg(f64, i64),
    Min(Option<Value>),
    Max(Option<Value>),
}
impl AggState {
    fn update(&mut self, expression: &ScalarExpr, data: &DataSet, row: usize) -> Result<()> {
        match (self, expression) {
            (Self::Value(slot), expr) => {
                if slot.is_none() {
                    *slot = Some(eval_expr(expr, &|index| data_value(data, index, row))?);
                }
            }
            (Self::Count(count), ScalarExpr::CountStar) => *count += 1,
            (Self::Count(count), ScalarExpr::Aggregate { expr, .. }) => {
                if !eval_expr(expr, &|index| data_value(data, index, row))?.is_null() {
                    *count += 1;
                }
            }
            (Self::SumInt(sum, seen), ScalarExpr::Aggregate { expr, .. }) => {
                if let Value::Int64(value) = eval_expr(expr, &|index| data_value(data, index, row))?
                {
                    *sum += value;
                    *seen = true;
                }
            }
            (Self::SumFloat(sum, seen), ScalarExpr::Aggregate { expr, .. }) => {
                match eval_expr(expr, &|index| data_value(data, index, row))? {
                    Value::Int64(value) => {
                        *sum += value as f64;
                        *seen = true;
                    }
                    Value::Float64(value) => {
                        *sum += value;
                        *seen = true;
                    }
                    _ => {}
                }
            }
            (Self::Avg(sum, count), ScalarExpr::Aggregate { expr, .. }) => {
                match eval_expr(expr, &|index| data_value(data, index, row))? {
                    Value::Int64(value) => {
                        *sum += value as f64;
                        *count += 1;
                    }
                    Value::Float64(value) => {
                        *sum += value;
                        *count += 1;
                    }
                    _ => {}
                }
            }
            (Self::Min(current), ScalarExpr::Aggregate { expr, .. }) => {
                let value = eval_expr(expr, &|index| data_value(data, index, row))?;
                if !value.is_null() && current.as_ref().is_none_or(|old| value < *old) {
                    *current = Some(value);
                }
            }
            (Self::Max(current), ScalarExpr::Aggregate { expr, .. }) => {
                let value = eval_expr(expr, &|index| data_value(data, index, row))?;
                if !value.is_null() && current.as_ref().is_none_or(|old| value > *old) {
                    *current = Some(value);
                }
            }
            _ => {
                return Err(Error::Execution(
                    "aggregate state/expression mismatch".into(),
                ));
            }
        }
        Ok(())
    }
    fn finish(self) -> Value {
        match self {
            Self::Value(v) | Self::Min(v) | Self::Max(v) => v.unwrap_or(Value::Null),
            Self::Count(v) => Value::Int64(v),
            Self::SumInt(v, true) => Value::Int64(v),
            Self::SumFloat(v, true) => Value::Float64(v),
            Self::Avg(sum, count) if count > 0 => Value::Float64(sum / count as f64),
            _ => Value::Null,
        }
    }
}
fn initial_states(expressions: &[NamedExpr]) -> Vec<AggState> {
    expressions
        .iter()
        .map(|expression| match &expression.expr {
            ScalarExpr::CountStar
            | ScalarExpr::Aggregate {
                function: AggregateFunction::Count,
                ..
            } => AggState::Count(0),
            ScalarExpr::Aggregate {
                function: AggregateFunction::Sum,
                data_type: DataType::Int64,
                ..
            } => AggState::SumInt(0, false),
            ScalarExpr::Aggregate {
                function: AggregateFunction::Sum,
                ..
            } => AggState::SumFloat(0.0, false),
            ScalarExpr::Aggregate {
                function: AggregateFunction::Avg,
                ..
            } => AggState::Avg(0.0, 0),
            ScalarExpr::Aggregate {
                function: AggregateFunction::Min,
                ..
            } => AggState::Min(None),
            ScalarExpr::Aggregate {
                function: AggregateFunction::Max,
                ..
            } => AggState::Max(None),
            _ => AggState::Value(None),
        })
        .collect()
}

fn nested_loop_join(left: &DataSet, right: &DataSet, on: &ScalarExpr) -> Result<DataSet> {
    let mut columns = vec![Vec::new(); left.columns.len() + right.columns.len()];
    for l in 0..left.row_count {
        for r in 0..right.row_count {
            let value = eval_expr(on, &|index| {
                if index < left.columns.len() {
                    data_value(left, index, l)
                } else {
                    data_value(right, index - left.columns.len(), r)
                }
            })?;
            if value.as_bool() == Some(true) {
                append_join_row(&mut columns, left, l, right, r);
            }
        }
    }
    joined_data(left, right, columns)
}

fn hash_join(left: &DataSet, right: &DataSet, on: &ScalarExpr) -> Result<DataSet> {
    let ScalarExpr::Binary {
        left: key_left,
        right: key_right,
        ..
    } = on
    else {
        return nested_loop_join(left, right, on);
    };
    let (ScalarExpr::Column { index: a, .. }, ScalarExpr::Column { index: b, .. }) =
        (&**key_left, &**key_right)
    else {
        return nested_loop_join(left, right, on);
    };
    let width = left.columns.len();
    let (left_key, right_key) = if *a < width && *b >= width {
        (*a, *b - width)
    } else if *b < width && *a >= width {
        (*b, *a - width)
    } else {
        return nested_loop_join(left, right, on);
    };
    let mut hash: HashMap<String, Vec<usize>> = HashMap::new();
    for row in 0..left.row_count {
        let value = data_value(left, left_key, row)?;
        if !value.is_null() {
            hash.entry(hash_key(&value)).or_default().push(row);
        }
    }
    let mut columns = vec![Vec::new(); left.columns.len() + right.columns.len()];
    for r in 0..right.row_count {
        let value = data_value(right, right_key, r)?;
        if value.is_null() {
            continue;
        }
        if let Some(matches) = hash.get(&hash_key(&value)) {
            for &l in matches {
                if eval_expr(on, &|index| {
                    if index < width {
                        data_value(left, index, l)
                    } else {
                        data_value(right, index - width, r)
                    }
                })?
                .as_bool()
                    == Some(true)
                {
                    append_join_row(&mut columns, left, l, right, r);
                }
            }
        }
    }
    joined_data(left, right, columns)
}
fn hash_key(value: &Value) -> String {
    match value {
        Value::Int64(v) => format!("n:{:.17}", *v as f64),
        Value::Float64(v) => format!("n:{v:.17}"),
        _ => format!("{value:?}"),
    }
}
fn append_join_row(
    columns: &mut [Vec<Value>],
    left: &DataSet,
    l: usize,
    right: &DataSet,
    r: usize,
) {
    for (i, column) in left.columns.iter().enumerate() {
        columns[i].push(column.get(l).cloned().unwrap_or(Value::Null));
    }
    for (i, column) in right.columns.iter().enumerate() {
        columns[left.columns.len() + i].push(column.get(r).cloned().unwrap_or(Value::Null));
    }
}
fn joined_data(left: &DataSet, right: &DataSet, columns: Vec<Vec<Value>>) -> Result<DataSet> {
    let mut schema = left.schema.clone();
    schema.extend(right.schema.clone());
    let row_count = columns.iter().find(|c| !c.is_empty()).map_or(0, Vec::len);
    Ok(DataSet {
        schema,
        columns,
        row_count,
    })
}

fn sort(data: &DataSet, keys: &[SortExpr]) -> Result<DataSet> {
    let mut indices = (0..data.row_count).collect::<Vec<_>>();
    let mut key_values = Vec::with_capacity(data.row_count);
    for row in 0..data.row_count {
        key_values.push(
            keys.iter()
                .map(|key| eval_expr(&key.expr, &|index| data_value(data, index, row)))
                .collect::<Result<Vec<_>>>()?,
        );
    }
    indices.sort_by(|a, b| {
        for (index, key) in keys.iter().enumerate() {
            let order = key_values[*a][index]
                .partial_cmp(&key_values[*b][index])
                .unwrap_or(Ordering::Equal);
            if order != Ordering::Equal {
                return if key.asc { order } else { order.reverse() };
            }
        }
        Ordering::Equal
    });
    Ok(take_rows(data, &indices))
}

fn eval_expr(expr: &ScalarExpr, get: &impl Fn(usize) -> Result<Value>) -> Result<Value> {
    match expr {
        ScalarExpr::Literal(value) => Ok(value.clone()),
        ScalarExpr::Column { index, .. } => get(*index),
        ScalarExpr::Unary { op, expr, .. } => eval_unary(*op, eval_expr(expr, get)?),
        ScalarExpr::Binary {
            left, op, right, ..
        } => eval_binary(eval_expr(left, get)?, *op, eval_expr(right, get)?),
        ScalarExpr::Aggregate { .. } | ScalarExpr::CountStar => Err(Error::Execution(
            "aggregate expression evaluated outside aggregate operator".into(),
        )),
    }
}
fn data_value(data: &DataSet, column: usize, row: usize) -> Result<Value> {
    data.columns
        .get(column)
        .and_then(|values| values.get(row))
        .cloned()
        .ok_or_else(|| {
            Error::Execution(format!(
                "column {column} was pruned but later required (planner bug)"
            ))
        })
}
fn take_rows(data: &DataSet, indices: &[usize]) -> DataSet {
    DataSet {
        schema: data.schema.clone(),
        columns: data
            .columns
            .iter()
            .map(|column| {
                if column.is_empty() {
                    Vec::new()
                } else {
                    indices.iter().map(|i| column[*i].clone()).collect()
                }
            })
            .collect(),
        row_count: indices.len(),
    }
}
fn value_bytes(value: &Value) -> usize {
    match value {
        Value::Int64(_) | Value::Float64(_) => 8,
        Value::Boolean(_) => 1,
        Value::Utf8(v) => v.len(),
        Value::Null => 0,
    }
}
fn count_scan_matches(table: &crate::storage::Table, predicates: &[ScalarExpr]) -> usize {
    (0..table.stats.row_count)
        .filter(|row| {
            predicates.iter().all(|predicate| {
                eval_expr(predicate, &|index| Ok(table.columns[index].value(*row)))
                    .ok()
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binder::Binder;
    use crate::optimizer::{OptimizerOptions, optimize};
    use crate::parse_sql;
    use crate::physical::{JoinPreference, create_physical_plan};
    use crate::storage::{Catalog, Table};
    use crate::types::Field;
    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.register(
            Table::from_rows(
                "sales",
                vec![
                    Field {
                        qualifier: None,
                        name: "region".into(),
                        data_type: DataType::Utf8,
                        nullable: false,
                    },
                    Field {
                        qualifier: None,
                        name: "amount".into(),
                        data_type: DataType::Int64,
                        nullable: false,
                    },
                ],
                vec![
                    vec![Value::Utf8("EU".into()), Value::Int64(10)],
                    vec![Value::Utf8("US".into()), Value::Int64(5)],
                    vec![Value::Utf8("EU".into()), Value::Int64(20)],
                ],
            )
            .unwrap(),
        );
        c
    }
    fn run(sql: &str) -> DataSet {
        let c = catalog();
        let logical = Binder::new(&c).bind(&parse_sql(sql).unwrap()).unwrap();
        let (optimized, _) = optimize(logical, OptimizerOptions::default()).unwrap();
        execute(
            &create_physical_plan(&optimized, JoinPreference::Auto),
            &ExecutionConfig::default(),
        )
        .unwrap()
        .data
    }
    #[test]
    fn filters_projects_sorts_and_limits() {
        let data = run("SELECT amount FROM sales WHERE amount > 5 ORDER BY amount DESC LIMIT 1");
        assert_eq!(data.rows(), vec![vec![Value::Int64(20)]]);
    }
    #[test]
    fn grouped_aggregate_executes() {
        let data = run(
            "SELECT region, SUM(amount) AS total FROM sales GROUP BY region ORDER BY total DESC",
        );
        assert_eq!(data.row_count, 2);
        assert_eq!(
            data.rows()[0],
            vec![Value::Utf8("EU".into()), Value::Int64(30)]
        );
    }
    #[test]
    fn tuple_and_vectorized_agree() {
        let c = catalog();
        let logical = Binder::new(&c)
            .bind(&parse_sql("SELECT amount * 2 AS x FROM sales WHERE amount >= 10").unwrap())
            .unwrap();
        let (optimized, _) = optimize(logical, OptimizerOptions::default()).unwrap();
        let p = create_physical_plan(&optimized, JoinPreference::Auto);
        let vector = execute(&p, &ExecutionConfig::default())
            .unwrap()
            .data
            .rows();
        let tuple = execute(
            &p,
            &ExecutionConfig {
                mode: ExecutionMode::Tuple,
                batch_size: 1,
            },
        )
        .unwrap()
        .data
        .rows();
        assert_eq!(vector, tuple);
    }
}
