use crate::types::{DataType, Field, Schema, Value};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"LAM1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ColumnData {
    Int64(Vec<Option<i64>>),
    Float64(Vec<Option<f64>>),
    Utf8(Vec<Option<String>>),
    Boolean(Vec<Option<bool>>),
}

impl ColumnData {
    pub fn empty(data_type: DataType) -> Result<Self> {
        Self::with_capacity(data_type, 0)
    }

    pub fn with_capacity(data_type: DataType, capacity: usize) -> Result<Self> {
        Ok(match data_type {
            DataType::Int64 => Self::Int64(Vec::with_capacity(capacity)),
            DataType::Float64 => Self::Float64(Vec::with_capacity(capacity)),
            DataType::Utf8 => Self::Utf8(Vec::with_capacity(capacity)),
            DataType::Boolean => Self::Boolean(Vec::with_capacity(capacity)),
            DataType::Null => {
                return Err(Error::Storage(
                    "NULL cannot be a physical column type".into(),
                ));
            }
        })
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Int64(v) => v.len(),
            Self::Float64(v) => v.len(),
            Self::Utf8(v) => v.len(),
            Self::Boolean(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn data_type(&self) -> DataType {
        match self {
            Self::Int64(_) => DataType::Int64,
            Self::Float64(_) => DataType::Float64,
            Self::Utf8(_) => DataType::Utf8,
            Self::Boolean(_) => DataType::Boolean,
        }
    }

    pub fn value(&self, index: usize) -> Value {
        match self {
            Self::Int64(v) => v[index].map(Value::Int64).unwrap_or(Value::Null),
            Self::Float64(v) => v[index].map(Value::Float64).unwrap_or(Value::Null),
            Self::Utf8(v) => v[index].clone().map(Value::Utf8).unwrap_or(Value::Null),
            Self::Boolean(v) => v[index].map(Value::Boolean).unwrap_or(Value::Null),
        }
    }

    pub fn push(&mut self, value: Value) -> Result<()> {
        match (self, value) {
            (Self::Int64(v), Value::Int64(x)) => v.push(Some(x)),
            (Self::Float64(v), Value::Float64(x)) => v.push(Some(x)),
            (Self::Float64(v), Value::Int64(x)) => v.push(Some(x as f64)),
            (Self::Utf8(v), Value::Utf8(x)) => v.push(Some(x)),
            (Self::Boolean(v), Value::Boolean(x)) => v.push(Some(x)),
            (Self::Int64(v), Value::Null) => v.push(None),
            (Self::Float64(v), Value::Null) => v.push(None),
            (Self::Utf8(v), Value::Null) => v.push(None),
            (Self::Boolean(v), Value::Null) => v.push(None),
            (column, value) => {
                return Err(Error::Storage(format!(
                    "cannot store {} in {} column",
                    value.data_type(),
                    column.data_type()
                )));
            }
        }
        Ok(())
    }

    pub fn push_from(&mut self, source: &Self, index: usize) -> Result<()> {
        match (self, source) {
            (Self::Int64(output), Self::Int64(input)) => output.push(input[index]),
            (Self::Float64(output), Self::Float64(input)) => output.push(input[index]),
            (Self::Utf8(output), Self::Utf8(input)) => output.push(input[index].clone()),
            (Self::Boolean(output), Self::Boolean(input)) => output.push(input[index]),
            (output, input) => {
                return Err(Error::Storage(format!(
                    "cannot copy {} values into {} column",
                    input.data_type(),
                    output.data_type()
                )));
            }
        }
        Ok(())
    }

    pub fn take(&self, indices: &[usize]) -> Self {
        match self {
            Self::Int64(values) => {
                Self::Int64(indices.iter().map(|index| values[*index]).collect())
            }
            Self::Float64(values) => {
                Self::Float64(indices.iter().map(|index| values[*index]).collect())
            }
            Self::Utf8(values) => {
                Self::Utf8(indices.iter().map(|index| values[*index].clone()).collect())
            }
            Self::Boolean(values) => {
                Self::Boolean(indices.iter().map(|index| values[*index]).collect())
            }
        }
    }

    pub fn estimated_bytes(&self) -> usize {
        match self {
            Self::Int64(v) => v.len() * std::mem::size_of::<Option<i64>>(),
            Self::Float64(v) => v.len() * std::mem::size_of::<Option<f64>>(),
            Self::Boolean(v) => v.len() * std::mem::size_of::<Option<bool>>(),
            Self::Utf8(v) => v
                .iter()
                .map(|x| x.as_ref().map_or(0, String::len) + std::mem::size_of::<Option<String>>())
                .sum(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnStats {
    pub min: Option<Value>,
    pub max: Option<Value>,
    pub cardinality: usize,
    pub null_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableStats {
    pub row_count: usize,
    pub columns: Vec<ColumnStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub schema: Schema,
    pub columns: Vec<ColumnData>,
    pub stats: TableStats,
}

impl Table {
    pub fn from_rows(
        name: impl Into<String>,
        schema: Schema,
        rows: Vec<Vec<Value>>,
    ) -> Result<Self> {
        let name = name.into();
        let mut columns = schema
            .iter()
            .map(|f| ColumnData::empty(f.data_type))
            .collect::<Result<Vec<_>>>()?;
        for (row_index, row) in rows.into_iter().enumerate() {
            if row.len() != schema.len() {
                return Err(Error::Storage(format!(
                    "row {row_index} has {} values; expected {}",
                    row.len(),
                    schema.len()
                )));
            }
            for (column, value) in columns.iter_mut().zip(row) {
                column.push(value)?;
            }
        }
        let stats = compute_stats(&columns);
        Ok(Self {
            name,
            schema,
            columns,
            stats,
        })
    }

    pub fn row(&self, index: usize) -> Vec<Value> {
        self.columns
            .iter()
            .map(|column| column.value(index))
            .collect()
    }

    pub fn estimated_bytes(&self) -> usize {
        self.columns.iter().map(ColumnData::estimated_bytes).sum()
    }

    pub fn write_columnar(&self, path: impl AsRef<Path>) -> Result<()> {
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(MAGIC)?;
        write_string(&mut writer, &self.name)?;
        write_u64(&mut writer, self.schema.len() as u64)?;
        write_u64(&mut writer, self.stats.row_count as u64)?;
        for (field, column) in self.schema.iter().zip(&self.columns) {
            write_string(&mut writer, &field.name)?;
            writer.write_all(&[type_tag(field.data_type), u8::from(field.nullable)])?;
            for row in 0..column.len() {
                write_value(&mut writer, &column.value(row), field.data_type)?;
            }
        }
        writer.flush()?;
        Ok(())
    }

    pub fn read_columnar(path: impl AsRef<Path>) -> Result<Self> {
        let mut reader = BufReader::new(File::open(path)?);
        let mut magic = [0; 4];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Storage(
                "not a Lamina columnar file (expected LAM1)".into(),
            ));
        }
        let name = read_string(&mut reader)?;
        let column_count = read_u64(&mut reader)? as usize;
        let row_count = read_u64(&mut reader)? as usize;
        let mut schema = Vec::with_capacity(column_count);
        let mut columns = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            let field_name = read_string(&mut reader)?;
            let mut tags = [0; 2];
            reader.read_exact(&mut tags)?;
            let data_type = tag_type(tags[0])?;
            let mut column = ColumnData::empty(data_type)?;
            for _ in 0..row_count {
                column.push(read_value(&mut reader, data_type)?)?;
            }
            schema.push(Field {
                qualifier: None,
                name: field_name,
                data_type,
                nullable: tags[1] != 0,
            });
            columns.push(column);
        }
        let stats = compute_stats(&columns);
        Ok(Self {
            name,
            schema,
            columns,
            stats,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct Catalog {
    tables: HashMap<String, Arc<Table>>,
}

impl Catalog {
    pub fn register(&mut self, table: Table) {
        self.tables
            .insert(table.name.to_ascii_lowercase(), Arc::new(table));
    }
    pub fn table(&self, name: &str) -> Option<Arc<Table>> {
        self.tables.get(&name.to_ascii_lowercase()).cloned()
    }
    pub fn table_names(&self) -> Vec<String> {
        let mut names = self.tables.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }
}

pub fn import_csv(path: impl AsRef<Path>, table_name: impl Into<String>) -> Result<Table> {
    let file = File::open(path)?;
    let mut lines = BufReader::new(file).lines();
    let header = lines
        .next()
        .ok_or_else(|| Error::Storage("CSV is empty".into()))??;
    let names = parse_csv_record(&header)?;
    if names.is_empty() {
        return Err(Error::Storage("CSV header has no columns".into()));
    }
    let mut records = Vec::new();
    for (index, line) in lines.enumerate() {
        let fields = parse_csv_record(&line?)?;
        if fields.len() != names.len() {
            return Err(Error::Storage(format!(
                "CSV row {} has {} fields; expected {}",
                index + 2,
                fields.len(),
                names.len()
            )));
        }
        records.push(fields);
    }
    let types = (0..names.len())
        .map(|column| infer_type(records.iter().map(|r| r[column].as_str())))
        .collect::<Vec<_>>();
    let schema = names
        .into_iter()
        .zip(&types)
        .map(|(name, data_type)| Field {
            qualifier: None,
            name,
            data_type: *data_type,
            nullable: true,
        })
        .collect();
    let rows = records
        .into_iter()
        .map(|record| {
            record
                .into_iter()
                .zip(&types)
                .map(|(raw, ty)| parse_value(&raw, *ty))
                .collect()
        })
        .collect::<Result<Vec<Vec<_>>>>()?;
    Table::from_rows(table_name, schema, rows)
}

fn parse_csv_record(line: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                result.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if quoted {
        return Err(Error::Storage("unterminated quoted CSV field".into()));
    }
    result.push(current);
    Ok(result)
}

fn infer_type<'a>(values: impl Iterator<Item = &'a str>) -> DataType {
    let mut ty = DataType::Null;
    for value in values.filter(|v| !v.is_empty()) {
        let candidate = if value.parse::<i64>().is_ok() {
            DataType::Int64
        } else if value.parse::<f64>().is_ok() {
            DataType::Float64
        } else if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
            DataType::Boolean
        } else {
            DataType::Utf8
        };
        ty = match (ty, candidate) {
            (DataType::Null, next) => next,
            (DataType::Int64, DataType::Float64) => DataType::Float64,
            (left, right) if left == right => left,
            _ => DataType::Utf8,
        };
    }
    if ty == DataType::Null {
        DataType::Utf8
    } else {
        ty
    }
}

fn parse_value(raw: &str, ty: DataType) -> Result<Value> {
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    let invalid = || Error::Storage(format!("cannot parse {raw:?} as {ty}"));
    match ty {
        DataType::Int64 => raw.parse().map(Value::Int64).map_err(|_| invalid()),
        DataType::Float64 => raw.parse().map(Value::Float64).map_err(|_| invalid()),
        DataType::Boolean => raw.parse().map(Value::Boolean).map_err(|_| invalid()),
        DataType::Utf8 => Ok(Value::Utf8(raw.into())),
        DataType::Null => Ok(Value::Null),
    }
}

fn compute_stats(columns: &[ColumnData]) -> TableStats {
    let row_count = columns.first().map_or(0, ColumnData::len);
    let columns = columns
        .iter()
        .map(|column| {
            let values = (0..column.len())
                .map(|i| column.value(i))
                .filter(|v| !v.is_null())
                .collect::<Vec<_>>();
            let null_count = column.len() - values.len();
            let cardinality = values.iter().cloned().collect::<HashSet<_>>().len();
            let min = values
                .iter()
                .cloned()
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let max = values
                .iter()
                .cloned()
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            ColumnStats {
                min,
                max,
                cardinality,
                null_count,
            }
        })
        .collect();
    TableStats { row_count, columns }
}

fn type_tag(ty: DataType) -> u8 {
    match ty {
        DataType::Int64 => 1,
        DataType::Float64 => 2,
        DataType::Utf8 => 3,
        DataType::Boolean => 4,
        DataType::Null => 0,
    }
}
fn tag_type(tag: u8) -> Result<DataType> {
    match tag {
        1 => Ok(DataType::Int64),
        2 => Ok(DataType::Float64),
        3 => Ok(DataType::Utf8),
        4 => Ok(DataType::Boolean),
        _ => Err(Error::Storage(format!("unknown type tag {tag}"))),
    }
}
fn write_u64(w: &mut impl Write, value: u64) -> Result<()> {
    w.write_all(&value.to_le_bytes())?;
    Ok(())
}
fn read_u64(r: &mut impl Read) -> Result<u64> {
    let mut bytes = [0; 8];
    r.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn write_string(w: &mut impl Write, value: &str) -> Result<()> {
    write_u64(w, value.len() as u64)?;
    w.write_all(value.as_bytes())?;
    Ok(())
}
fn read_string(r: &mut impl Read) -> Result<String> {
    let len = read_u64(r)? as usize;
    let mut bytes = vec![0; len];
    r.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|e| Error::Storage(e.to_string()))
}
fn write_value(w: &mut impl Write, value: &Value, ty: DataType) -> Result<()> {
    if value.is_null() {
        w.write_all(&[0])?;
        return Ok(());
    }
    w.write_all(&[1])?;
    match (value, ty) {
        (Value::Int64(v), DataType::Int64) => w.write_all(&v.to_le_bytes())?,
        (Value::Float64(v), DataType::Float64) => w.write_all(&v.to_le_bytes())?,
        (Value::Utf8(v), DataType::Utf8) => write_string(w, v)?,
        (Value::Boolean(v), DataType::Boolean) => w.write_all(&[u8::from(*v)])?,
        _ => {
            return Err(Error::Storage(
                "column value/type mismatch during write".into(),
            ));
        }
    }
    Ok(())
}
fn read_value(r: &mut impl Read, ty: DataType) -> Result<Value> {
    let mut present = [0];
    r.read_exact(&mut present)?;
    if present[0] == 0 {
        return Ok(Value::Null);
    }
    Ok(match ty {
        DataType::Int64 => {
            let mut b = [0; 8];
            r.read_exact(&mut b)?;
            Value::Int64(i64::from_le_bytes(b))
        }
        DataType::Float64 => {
            let mut b = [0; 8];
            r.read_exact(&mut b)?;
            Value::Float64(f64::from_le_bytes(b))
        }
        DataType::Utf8 => Value::Utf8(read_string(r)?),
        DataType::Boolean => {
            let mut b = [0];
            r.read_exact(&mut b)?;
            Value::Boolean(b[0] != 0)
        }
        DataType::Null => return Err(Error::Storage("NULL physical column".into())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> Schema {
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
                nullable: true,
            },
        ]
    }
    #[test]
    fn computes_column_statistics() {
        let table = Table::from_rows(
            "t",
            schema(),
            vec![
                vec![Value::Int64(2), Value::Utf8("b".into())],
                vec![Value::Int64(1), Value::Null],
            ],
        )
        .unwrap();
        assert_eq!(table.stats.row_count, 2);
        assert_eq!(table.stats.columns[0].min, Some(Value::Int64(1)));
        assert_eq!(table.stats.columns[1].null_count, 1);
    }
    #[test]
    fn columnar_round_trip() {
        let table = Table::from_rows(
            "t",
            schema(),
            vec![vec![Value::Int64(7), Value::Utf8("Ada".into())]],
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!("lamina-{}.lam", std::process::id()));
        table.write_columnar(&path).unwrap();
        let loaded = Table::read_columnar(&path).unwrap();
        std::fs::remove_file(path).ok();
        assert_eq!(loaded.row(0), table.row(0));
        assert_eq!(loaded.stats.row_count, 1);
    }
    #[test]
    fn csv_parser_handles_quotes() {
        assert_eq!(
            parse_csv_record("1,\"a,b\",\"x\"\"y\"").unwrap(),
            vec!["1", "a,b", "x\"y"]
        );
    }
}
