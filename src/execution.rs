use crate::ast::{AggregateFunction, BinaryOp, UnaryOp};
use crate::logical::{NamedExpr, ScalarExpr, SortExpr};
use crate::optimizer::{eval_binary, eval_unary};
use crate::physical::{JoinAlgorithm, PhysicalPlan};
use crate::storage::ColumnData;
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
    pub columns: Vec<Option<ColumnData>>,
    pub row_count: usize,
}

impl DataSet {
    pub fn row(&self, index: usize) -> Vec<Value> {
        self.columns
            .iter()
            .map(|column| {
                column
                    .as_ref()
                    .map_or(Value::Null, |values| values.value(index))
            })
            .collect()
    }
    pub fn rows(&self) -> Vec<Vec<Value>> {
        (0..self.row_count).map(|index| self.row(index)).collect()
    }
    pub fn estimated_bytes(&self) -> usize {
        self.columns
            .iter()
            .flatten()
            .map(ColumnData::estimated_bytes)
            .sum()
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
            let mut columns = vec![None; schema.len()];
            let row_count = if predicates.is_empty() {
                for &index in read_columns {
                    columns[index] = Some(table.columns[index].clone());
                }
                table.stats.row_count
            } else {
                let chunk = effective_batch(config);
                let mut selected = Vec::with_capacity(table.stats.row_count / 10);
                for start in (0..table.stats.row_count).step_by(chunk) {
                    let end = (start + chunk).min(table.stats.row_count);
                    selected.extend((start..end).filter(|row| {
                        predicates.iter().all(|predicate| {
                            fast_predicate(predicate, *row, &|index| table.columns.get(index))
                                .unwrap_or_else(|| {
                                    eval_expr(predicate, &|index| {
                                        Ok(table.columns[index].value(*row))
                                    })
                                    .ok()
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false)
                                })
                        })
                    }));
                }
                for &index in read_columns {
                    columns[index] = Some(table.columns[index].take(&selected));
                }
                selected.len()
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
            let keep = fast_predicate(predicate, row, &|index| {
                data.columns.get(index).and_then(Option::as_ref)
            })
            .map_or_else(
                || {
                    eval_expr(predicate, &|index| data_value(data, index, row))
                        .map(|value| value.as_bool() == Some(true))
                },
                Ok,
            )?;
            if keep {
                selected.push(row);
            }
        }
    }
    Ok(selected)
}

fn fast_predicate<'a>(
    expression: &ScalarExpr,
    row: usize,
    column: &impl Fn(usize) -> Option<&'a ColumnData>,
) -> Option<bool> {
    match expression {
        ScalarExpr::Literal(Value::Boolean(value)) => Some(*value),
        ScalarExpr::Unary {
            op: UnaryOp::Not,
            expr,
            ..
        } => fast_predicate(expr, row, column).map(|value| !value),
        ScalarExpr::Binary {
            left,
            op: BinaryOp::And,
            right,
            ..
        } => Some(fast_predicate(left, row, column)? && fast_predicate(right, row, column)?),
        ScalarExpr::Binary {
            left,
            op: BinaryOp::Or,
            right,
            ..
        } => Some(fast_predicate(left, row, column)? || fast_predicate(right, row, column)?),
        ScalarExpr::Binary {
            left, op, right, ..
        } if is_comparison(*op) => match (&**left, &**right) {
            (ScalarExpr::Column { index, .. }, ScalarExpr::Literal(value)) => {
                Some(compare_column_literal(column(*index)?, row, *op, value))
            }
            (ScalarExpr::Literal(value), ScalarExpr::Column { index, .. }) => Some(
                compare_column_literal(column(*index)?, row, reverse_comparison(*op), value),
            ),
            _ => None,
        },
        _ => None,
    }
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
    )
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

fn compare_column_literal(column: &ColumnData, row: usize, op: BinaryOp, literal: &Value) -> bool {
    match (column, literal) {
        (ColumnData::Int64(values), Value::Int64(right)) => {
            values[row].is_some_and(|left| compare_ordered(left, *right, op))
        }
        (ColumnData::Int64(values), Value::Float64(right)) => {
            values[row].is_some_and(|left| compare_ordered(left as f64, *right, op))
        }
        (ColumnData::Float64(values), Value::Int64(right)) => {
            values[row].is_some_and(|left| compare_ordered(left, *right as f64, op))
        }
        (ColumnData::Float64(values), Value::Float64(right)) => {
            values[row].is_some_and(|left| compare_ordered(left, *right, op))
        }
        (ColumnData::Utf8(values), Value::Utf8(right)) => values[row]
            .as_ref()
            .is_some_and(|left| compare_ordered(left.as_str(), right.as_str(), op)),
        (ColumnData::Boolean(values), Value::Boolean(right)) => {
            values[row].is_some_and(|left| compare_ordered(left, *right, op))
        }
        (_, Value::Null) => false,
        _ => false,
    }
}

fn compare_ordered<T: PartialEq + PartialOrd>(left: T, right: T, op: BinaryOp) -> bool {
    match op {
        BinaryOp::Eq => left == right,
        BinaryOp::NotEq => left != right,
        BinaryOp::Lt => left < right,
        BinaryOp::LtEq => left <= right,
        BinaryOp::Gt => left > right,
        BinaryOp::GtEq => left >= right,
        _ => false,
    }
}

fn project(data: &DataSet, expressions: &[NamedExpr], batch: usize) -> Result<DataSet> {
    let mut columns = Vec::with_capacity(expressions.len());
    for expression in expressions {
        if let ScalarExpr::Column { index, .. } = expression.expr {
            columns.push(Some(data_column(data, index)?.clone()));
            continue;
        }
        let mut column = ColumnData::with_capacity(expression.expr.data_type(), data.row_count)
            .map_err(|error| Error::Execution(error.to_string()))?;
        for start in (0..data.row_count).step_by(batch) {
            let end = (start + batch).min(data.row_count);
            for row in start..end {
                column
                    .push(eval_expr(&expression.expr, &|index| {
                        data_value(data, index, row)
                    })?)
                    .map_err(|error| Error::Execution(error.to_string()))?;
            }
        }
        columns.push(Some(column));
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
    let row_count = entries.len();
    let mut columns = expressions
        .iter()
        .map(|expression| {
            ColumnData::with_capacity(expression.expr.data_type(), row_count)
                .map(Some)
                .map_err(|error| Error::Execution(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    for (_, states) in entries {
        for (column, state) in columns.iter_mut().zip(states) {
            column
                .as_mut()
                .expect("aggregate columns are materialized")
                .push(state.finish())
                .map_err(|error| Error::Execution(error.to_string()))?;
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
    let mut columns = empty_join_columns(left, right, 0)?;
    let mut row_count = 0;
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
                append_join_row(&mut columns, left, l, right, r)?;
                row_count += 1;
            }
        }
    }
    Ok(joined_data(left, right, columns, row_count))
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
    let mut hash: HashMap<JoinKey, Vec<usize>> = HashMap::new();
    for row in 0..left.row_count {
        if let Some(key) = join_key_at(data_column(left, left_key)?, row) {
            hash.entry(key).or_default().push(row);
        }
    }
    let mut columns = empty_join_columns(left, right, right.row_count)?;
    let mut row_count = 0;
    for r in 0..right.row_count {
        let Some(key) = join_key_at(data_column(right, right_key)?, r) else {
            continue;
        };
        if let Some(matches) = hash.get(&key) {
            for &l in matches {
                if columns_equal(
                    data_column(left, left_key)?,
                    l,
                    data_column(right, right_key)?,
                    r,
                ) {
                    append_join_row(&mut columns, left, l, right, r)?;
                    row_count += 1;
                }
            }
        }
    }
    Ok(joined_data(left, right, columns, row_count))
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum JoinKey {
    Numeric(u64),
    Utf8(String),
    Boolean(bool),
}

fn join_key_at(column: &ColumnData, row: usize) -> Option<JoinKey> {
    match column {
        ColumnData::Int64(values) => {
            values[row].map(|value| JoinKey::Numeric(normalized_float_bits(value as f64)))
        }
        ColumnData::Float64(values) => {
            values[row].map(|value| JoinKey::Numeric(normalized_float_bits(value)))
        }
        ColumnData::Utf8(values) => values[row].clone().map(JoinKey::Utf8),
        ColumnData::Boolean(values) => values[row].map(JoinKey::Boolean),
    }
}

fn normalized_float_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn columns_equal(left: &ColumnData, left_row: usize, right: &ColumnData, right_row: usize) -> bool {
    match (left, right) {
        (ColumnData::Int64(a), ColumnData::Int64(b)) => {
            matches!((a[left_row], b[right_row]), (Some(a), Some(b)) if a == b)
        }
        (ColumnData::Float64(a), ColumnData::Float64(b)) => {
            matches!((a[left_row], b[right_row]), (Some(a), Some(b)) if a == b)
        }
        (ColumnData::Int64(a), ColumnData::Float64(b)) => {
            matches!((a[left_row], b[right_row]), (Some(a), Some(b)) if a as f64 == b)
        }
        (ColumnData::Float64(a), ColumnData::Int64(b)) => {
            matches!((a[left_row], b[right_row]), (Some(a), Some(b)) if a == b as f64)
        }
        (ColumnData::Utf8(a), ColumnData::Utf8(b)) => {
            matches!((&a[left_row], &b[right_row]), (Some(a), Some(b)) if a == b)
        }
        (ColumnData::Boolean(a), ColumnData::Boolean(b)) => {
            matches!((a[left_row], b[right_row]), (Some(a), Some(b)) if a == b)
        }
        _ => false,
    }
}
fn append_join_row(
    columns: &mut [Option<ColumnData>],
    left: &DataSet,
    l: usize,
    right: &DataSet,
    r: usize,
) -> Result<()> {
    for (i, column) in left.columns.iter().enumerate() {
        if let (Some(output), Some(input)) = (&mut columns[i], column) {
            output
                .push_from(input, l)
                .map_err(|error| Error::Execution(error.to_string()))?;
        }
    }
    for (i, column) in right.columns.iter().enumerate() {
        if let (Some(output), Some(input)) = (&mut columns[left.columns.len() + i], column) {
            output
                .push_from(input, r)
                .map_err(|error| Error::Execution(error.to_string()))?;
        }
    }
    Ok(())
}
fn empty_join_columns(
    left: &DataSet,
    right: &DataSet,
    capacity: usize,
) -> Result<Vec<Option<ColumnData>>> {
    left.columns
        .iter()
        .chain(&right.columns)
        .map(|column| {
            column
                .as_ref()
                .map(|values| {
                    ColumnData::with_capacity(values.data_type(), capacity)
                        .map_err(|error| Error::Execution(error.to_string()))
                })
                .transpose()
        })
        .collect()
}
fn joined_data(
    left: &DataSet,
    right: &DataSet,
    columns: Vec<Option<ColumnData>>,
    row_count: usize,
) -> DataSet {
    let mut schema = left.schema.clone();
    schema.extend(right.schema.clone());
    DataSet {
        schema,
        columns,
        row_count,
    }
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
    Ok(data_column(data, column)?.value(row))
}
fn data_column(data: &DataSet, column: usize) -> Result<&ColumnData> {
    data.columns
        .get(column)
        .and_then(Option::as_ref)
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
            .map(|column| column.as_ref().map(|values| values.take(indices)))
            .collect(),
        row_count: indices.len(),
    }
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

    #[test]
    fn typed_filter_kernel_handles_reversed_and_null_comparisons() {
        assert_eq!(
            run("SELECT amount FROM sales WHERE 15 < amount").rows(),
            vec![vec![Value::Int64(20)]]
        );
        assert_eq!(
            run("SELECT amount FROM sales WHERE amount = NULL").row_count,
            0
        );
    }
}
