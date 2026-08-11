use lamina_sql::execution::{DataSet, ExecutionMode};
use lamina_sql::storage::Table;
use lamina_sql::types::{DataType, Field, Value};
use lamina_sql::{Engine, QueryResult, Result, parse_sql};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("demo") => demo(),
        Some("ast") => {
            let sql = args.collect::<Vec<_>>().join(" ");
            let statement = parse_sql(&sql)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&statement).expect("serializable AST")
            );
            Ok(())
        }
        Some("query") => query(args.collect()),
        Some("import") => {
            let csv = required(args.next(), "CSV input path")?;
            let output = required(args.next(), "LAM1 output path")?;
            let name = args.next().unwrap_or_else(|| "data".into());
            let table = lamina_sql::storage::import_csv(csv, name)?;
            table.write_columnar(output)?;
            println!(
                "wrote {} rows and {} columns",
                table.stats.row_count,
                table.schema.len()
            );
            Ok(())
        }
        _ => {
            print_help();
            Ok(())
        }
    }
}

fn query(args: Vec<String>) -> Result<()> {
    let mut engine = Engine::default();
    let mut index = 0;
    while args.get(index).is_some_and(|arg| {
        arg == "--csv" || arg == "--columnar" || arg == "--batch-size" || arg == "--tuple"
    }) {
        match args[index].as_str() {
            "--csv" => {
                let spec = required(args.get(index + 1).cloned(), "name=path after --csv")?;
                let (name, path) = split_spec(&spec)?;
                engine.import_csv(name, path)?;
                index += 2;
            }
            "--columnar" => {
                let spec = required(args.get(index + 1).cloned(), "name=path after --columnar")?;
                let (name, path) = split_spec(&spec)?;
                let mut table = Table::read_columnar(path)?;
                table.name = name.into();
                engine.register(table);
                index += 2;
            }
            "--batch-size" => {
                engine.config.execution.batch_size =
                    required(args.get(index + 1).cloned(), "integer after --batch-size")?
                        .parse()
                        .map_err(|_| lamina_sql::Error::Execution("invalid batch size".into()))?;
                index += 2;
            }
            "--tuple" => {
                engine.config.execution.mode = ExecutionMode::Tuple;
                index += 1;
            }
            _ => unreachable!(),
        }
    }
    let sql = args[index..].join(" ");
    show_result(engine.query(&sql)?, false)
}

fn demo() -> Result<()> {
    let mut engine = Engine::default();
    engine.register(Table::from_rows(
        "customers",
        vec![
            field("customer_id", DataType::Int64),
            field("region", DataType::Utf8),
            field("segment", DataType::Utf8),
        ],
        vec![
            vec![Value::Int64(1), text("EUROPE"), text("BUILDING")],
            vec![Value::Int64(2), text("AMERICA"), text("AUTOMOBILE")],
            vec![Value::Int64(3), text("EUROPE"), text("MACHINERY")],
            vec![Value::Int64(4), text("ASIA"), text("BUILDING")],
        ],
    )?);
    let order_rows = (1..=200)
        .map(|order_id| {
            vec![
                Value::Int64(1000 + order_id),
                Value::Int64((order_id % 4) + 1),
                Value::Int64(20 + (order_id % 20) * 10),
            ]
        })
        .collect();
    engine.register(Table::from_rows(
        "orders",
        vec![
            field("order_id", DataType::Int64),
            field("customer_id", DataType::Int64),
            field("total", DataType::Int64),
        ],
        order_rows,
    )?);
    let sql = "SELECT c.region, SUM(o.total) AS revenue FROM customers c INNER JOIN orders o ON c.customer_id = o.customer_id WHERE o.total >= 80 GROUP BY c.region ORDER BY revenue DESC LIMIT 3";
    println!("LAMINA SQL — query pipeline demo\n\nSQL\n  {sql}\n");
    show_result(engine.query(sql)?, true)
}

fn show_result(result: QueryResult, trace: bool) -> Result<()> {
    match result {
        QueryResult::Explain(plan) => println!("{plan}"),
        QueryResult::Data {
            execution,
            trace: stages,
        } => {
            if trace {
                println!("LOGICAL PLAN\n{}", stages.logical_plan);
                println!("OPTIMIZED LOGICAL PLAN\n{}", stages.optimized_logical_plan);
                println!(
                    "OPTIMIZER ACTIONS\n{}\n",
                    serde_json::to_string_pretty(&stages.optimization).unwrap()
                );
                println!("PHYSICAL PLAN\n{}", stages.physical_plan);
            }
            print_table(&execution.data);
            println!("\nEXECUTION PROFILE\n{}", execution.profile.format_tree());
        }
    }
    Ok(())
}

fn print_table(data: &DataSet) {
    let mut widths = data
        .schema
        .iter()
        .map(|field| field.name.len())
        .collect::<Vec<_>>();
    for row in data.rows() {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.to_string().len());
        }
    }
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|w| "-".repeat(*w + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    println!("{border}");
    println!(
        "|{}|",
        data.schema
            .iter()
            .zip(&widths)
            .map(|(field, width)| format!(" {:width$} ", field.name, width = *width))
            .collect::<Vec<_>>()
            .join("|")
    );
    println!("{border}");
    for row in data.rows() {
        println!(
            "|{}|",
            row.iter()
                .zip(&widths)
                .map(|(value, width)| format!(" {:width$} ", value, width = *width))
                .collect::<Vec<_>>()
                .join("|")
        );
    }
    println!("{border}\n{} row(s)", data.row_count);
}

fn field(name: &str, data_type: DataType) -> Field {
    Field {
        qualifier: None,
        name: name.into(),
        data_type,
        nullable: false,
    }
}
fn text(value: &str) -> Value {
    Value::Utf8(value.into())
}
fn required(value: Option<String>, what: &str) -> Result<String> {
    value.ok_or_else(|| lamina_sql::Error::Execution(format!("missing {what}")))
}
fn split_spec(spec: &str) -> Result<(&str, PathBuf)> {
    let (name, path) = spec
        .split_once('=')
        .ok_or_else(|| lamina_sql::Error::Execution("table input must be name=path".into()))?;
    Ok((name, PathBuf::from(path)))
}
fn print_help() {
    println!(
        "Lamina SQL\n\n  lamina demo\n  lamina ast <SQL>\n  lamina query [--csv name=path] [--columnar name=path] [--batch-size N] [--tuple] <SQL>\n  lamina import <input.csv> <output.lam> [table_name]"
    );
}
