use lamina_sql::execution::ExecutionMode;
use lamina_sql::physical::JoinPreference;
use lamina_sql::storage::Table;
use lamina_sql::types::{DataType, Field, Value};
use lamina_sql::{Engine, QueryResult, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Serialize)]
struct Environment {
    timestamp_utc: String,
    operating_system: String,
    architecture: String,
    processor: String,
    logical_cpus: usize,
    rustc: String,
    cargo: String,
    commit: String,
    profile: String,
    cpu_utilization: String,
}

#[derive(Serialize, Clone)]
struct Measurement {
    experiment: String,
    variant: String,
    iteration: usize,
    rows_input: usize,
    rows_output: usize,
    end_to_end_ns: u128,
    execution_ns: u128,
    output_memory_bytes: usize,
    configuration: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct BenchmarkOutput {
    environment: Environment,
    generator: BTreeMap<String, String>,
    measurements: Vec<Measurement>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let (rows, iterations, output) = arguments()?;
    let (customer, orders, lineitem, row_store) = generate(rows);
    let mut measurements = Vec::new();

    benchmark_storage(&lineitem, &row_store, iterations, &mut measurements);
    run_query_case(
        "B_predicate_pushdown",
        "on",
        rows,
        iterations,
        engine(&customer, &orders, &lineitem),
        "SELECT l_orderkey, l_extendedprice FROM lineitem WHERE l_quantity < 10",
        BTreeMap::new(),
        &mut measurements,
    )?;
    let mut no_pushdown = engine(&customer, &orders, &lineitem);
    no_pushdown.config.optimizer.predicate_pushdown = false;
    run_query_case(
        "B_predicate_pushdown",
        "off",
        rows,
        iterations,
        no_pushdown,
        "SELECT l_orderkey, l_extendedprice FROM lineitem WHERE l_quantity < 10",
        BTreeMap::new(),
        &mut measurements,
    )?;

    run_query_case(
        "C_projection_pruning",
        "on",
        rows,
        iterations,
        engine(&customer, &orders, &lineitem),
        "SELECT l_orderkey, l_extendedprice FROM lineitem",
        BTreeMap::new(),
        &mut measurements,
    )?;
    let mut no_pruning = engine(&customer, &orders, &lineitem);
    no_pruning.config.optimizer.projection_pruning = false;
    run_query_case(
        "C_projection_pruning",
        "off",
        rows,
        iterations,
        no_pruning,
        "SELECT l_orderkey, l_extendedprice FROM lineitem",
        BTreeMap::new(),
        &mut measurements,
    )?;

    for (variant, preference) in [
        ("nested_loop", JoinPreference::NestedLoop),
        ("hash", JoinPreference::Hash),
    ] {
        let mut configured = engine(&customer, &orders, &lineitem);
        configured.config.join_preference = preference;
        run_query_case(
            "D_join_algorithm",
            variant,
            rows,
            iterations,
            configured,
            "SELECT o.o_orderkey, c.c_region FROM orders o INNER JOIN customer c ON o.o_custkey = c.c_custkey WHERE o.o_totalprice > 1000 LIMIT 2000",
            BTreeMap::new(),
            &mut measurements,
        )?;
    }
    for (variant, mode) in [
        ("tuple", ExecutionMode::Tuple),
        ("vectorized", ExecutionMode::Vectorized),
    ] {
        let mut configured = engine(&customer, &orders, &lineitem);
        configured.config.execution.mode = mode;
        run_query_case(
            "E_execution_model",
            variant,
            rows,
            iterations,
            configured,
            "SELECT l_extendedprice * 2 AS value FROM lineitem WHERE l_quantity < 25",
            BTreeMap::new(),
            &mut measurements,
        )?;
    }
    for batch_size in [1, 16, 64, 256, 1024, 4096] {
        let mut configured = engine(&customer, &orders, &lineitem);
        configured.config.execution.batch_size = batch_size;
        let mut config = BTreeMap::new();
        config.insert("batch_size".into(), batch_size.to_string());
        run_query_case(
            "F_batch_size",
            &batch_size.to_string(),
            rows,
            iterations,
            configured,
            "SELECT l_extendedprice * 2 AS value FROM lineitem WHERE l_quantity < 25",
            config,
            &mut measurements,
        )?;
    }

    let mut generator = BTreeMap::new();
    generator.insert(
        "description".into(),
        "deterministic TPC-H-shaped synthetic data; not official dbgen output".into(),
    );
    generator.insert("lineitem_rows".into(), rows.to_string());
    generator.insert("seed".into(), "formula-v1".into());
    let output_data = BenchmarkOutput {
        environment: environment(),
        generator,
        measurements,
    };
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("raw.json"),
        serde_json::to_string_pretty(&output_data).unwrap(),
    )?;
    write_csv(&output.join("raw.csv"), &output_data.measurements)?;
    let summary = summarize(&output_data.measurements);
    fs::write(
        output.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )?;
    fs::write(output.join("results.svg"), render_svg(&summary))?;
    println!(
        "wrote {} measurements to {}",
        output_data.measurements.len(),
        output.display()
    );
    Ok(())
}

fn arguments() -> Result<(usize, usize, PathBuf)> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut rows = 50_000;
    let mut iterations = 5;
    let mut output = PathBuf::from("benchmarks/results/latest");
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--rows" => {
                rows = args
                    .get(index + 1)
                    .ok_or_else(|| lamina_sql::Error::Execution("--rows needs a value".into()))?
                    .parse()
                    .map_err(|_| lamina_sql::Error::Execution("invalid --rows".into()))?;
                index += 2;
            }
            "--iterations" => {
                iterations = args
                    .get(index + 1)
                    .ok_or_else(|| {
                        lamina_sql::Error::Execution("--iterations needs a value".into())
                    })?
                    .parse()
                    .map_err(|_| lamina_sql::Error::Execution("invalid --iterations".into()))?;
                index += 2;
            }
            "--out" => {
                output = args
                    .get(index + 1)
                    .map(PathBuf::from)
                    .ok_or_else(|| lamina_sql::Error::Execution("--out needs a path".into()))?;
                index += 2;
            }
            value => {
                return Err(lamina_sql::Error::Execution(format!(
                    "unknown benchmark option {value}"
                )));
            }
        }
    }
    Ok((rows, iterations, output))
}

fn field(name: &str, data_type: DataType) -> Field {
    Field {
        qualifier: None,
        name: name.into(),
        data_type,
        nullable: false,
    }
}
fn generate(rows: usize) -> (Table, Table, Table, Vec<Vec<Value>>) {
    let customer_count = (rows / 20).max(10);
    let order_count = (rows / 4).max(20);
    let customer_rows = (1..=customer_count)
        .map(|id| {
            vec![
                Value::Int64(id as i64),
                Value::Utf8(["AFRICA", "AMERICA", "ASIA", "EUROPE"][id % 4].into()),
            ]
        })
        .collect();
    let order_rows = (1..=order_count)
        .map(|id| {
            vec![
                Value::Int64(id as i64),
                Value::Int64((id % customer_count + 1) as i64),
                Value::Float64(((id * 7919) % 500_000) as f64 / 10.0),
            ]
        })
        .collect();
    let row_store = (0..rows)
        .map(|id| {
            vec![
                Value::Int64((id % order_count + 1) as i64),
                Value::Int64((id % 50 + 1) as i64),
                Value::Float64(((id * 104729) % 100_000) as f64 / 100.0),
                Value::Float64((id % 10) as f64 / 100.0),
                Value::Utf8(if id % 3 == 0 { "N" } else { "R" }.into()),
                Value::Int64((id % 7) as i64),
            ]
        })
        .collect::<Vec<_>>();
    let customer = Table::from_rows(
        "customer",
        vec![
            field("c_custkey", DataType::Int64),
            field("c_region", DataType::Utf8),
        ],
        customer_rows,
    )
    .unwrap();
    let orders = Table::from_rows(
        "orders",
        vec![
            field("o_orderkey", DataType::Int64),
            field("o_custkey", DataType::Int64),
            field("o_totalprice", DataType::Float64),
        ],
        order_rows,
    )
    .unwrap();
    let lineitem = Table::from_rows(
        "lineitem",
        vec![
            field("l_orderkey", DataType::Int64),
            field("l_quantity", DataType::Int64),
            field("l_extendedprice", DataType::Float64),
            field("l_discount", DataType::Float64),
            field("l_returnflag", DataType::Utf8),
            field("l_shipmode", DataType::Int64),
        ],
        row_store.clone(),
    )
    .unwrap();
    (customer, orders, lineitem, row_store)
}

fn engine(customer: &Table, orders: &Table, lineitem: &Table) -> Engine {
    let mut engine = Engine::default();
    engine.register(customer.clone());
    engine.register(orders.clone());
    engine.register(lineitem.clone());
    engine
}

#[allow(clippy::too_many_arguments)] // Benchmark cases are clearer at call sites with explicit dimensions.
fn run_query_case(
    experiment: &str,
    variant: &str,
    rows: usize,
    iterations: usize,
    engine: Engine,
    sql: &str,
    configuration: BTreeMap<String, String>,
    output: &mut Vec<Measurement>,
) -> Result<()> {
    for _ in 0..2 {
        black_box(engine.query(sql)?);
    }
    for iteration in 0..iterations {
        let start = Instant::now();
        let result = engine.query(sql)?;
        let elapsed = start.elapsed().as_nanos();
        let QueryResult::Data { execution, .. } = result else {
            unreachable!()
        };
        output.push(Measurement {
            experiment: experiment.into(),
            variant: variant.into(),
            iteration,
            rows_input: rows,
            rows_output: execution.data.row_count,
            end_to_end_ns: elapsed,
            execution_ns: execution.profile.elapsed_ns,
            output_memory_bytes: execution.profile.memory_bytes,
            configuration: configuration.clone(),
        });
    }
    Ok(())
}

fn benchmark_storage(
    column: &Table,
    rows: &[Vec<Value>],
    iterations: usize,
    output: &mut Vec<Measurement>,
) {
    for variant in ["row", "column"] {
        for iteration in 0..iterations {
            let start = Instant::now();
            let mut sum = 0.0;
            if variant == "row" {
                for row in rows {
                    if matches!(row[1], Value::Int64(v) if v < 10)
                        && let Value::Float64(v) = row[2]
                    {
                        sum += v;
                    }
                }
            } else {
                for index in 0..column.stats.row_count {
                    if matches!(column.columns[1].value(index), Value::Int64(v) if v < 10)
                        && let Value::Float64(v) = column.columns[2].value(index)
                    {
                        sum += v;
                    }
                }
            }
            black_box(sum);
            let elapsed = start.elapsed().as_nanos();
            output.push(Measurement {
                experiment: "A_storage_layout".into(),
                variant: variant.into(),
                iteration,
                rows_input: rows.len(),
                rows_output: rows.len(),
                end_to_end_ns: elapsed,
                execution_ns: elapsed,
                output_memory_bytes: if variant == "row" {
                    rows.iter().flatten().map(std::mem::size_of_val).sum()
                } else {
                    column.estimated_bytes()
                },
                configuration: BTreeMap::new(),
            });
        }
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unavailable".into())
}
fn environment() -> Environment {
    Environment {
        timestamp_utc: command_output(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-Date).ToUniversalTime().ToString('o')",
            ],
        ),
        operating_system: format!(
            "{} {}",
            std::env::consts::OS,
            command_output("cmd", &["/C", "ver"])
        ),
        architecture: std::env::consts::ARCH.into(),
        processor: std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unavailable".into()),
        logical_cpus: std::thread::available_parallelism().map_or(0, usize::from),
        rustc: command_output("rustc", &["--version"]),
        cargo: command_output("cargo", &["--version"]),
        commit: command_output("git", &["rev-parse", "HEAD"]),
        profile: "release (thin LTO, debug symbols)".into(),
        cpu_utilization: "not sampled; wall-clock and estimated output allocation are recorded"
            .into(),
    }
}

fn summarize(measurements: &[Measurement]) -> BTreeMap<String, BTreeMap<String, u128>> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<u128>>> = BTreeMap::new();
    for m in measurements {
        grouped
            .entry(m.experiment.clone())
            .or_default()
            .entry(m.variant.clone())
            .or_default()
            .push(m.execution_ns);
    }
    grouped
        .into_iter()
        .map(|(experiment, variants)| {
            (
                experiment,
                variants
                    .into_iter()
                    .map(|(variant, mut values)| {
                        values.sort();
                        (variant, values[values.len() / 2])
                    })
                    .collect(),
            )
        })
        .collect()
}
fn write_csv(path: &Path, values: &[Measurement]) -> Result<()> {
    let mut csv = "experiment,variant,iteration,rows_input,rows_output,end_to_end_ns,execution_ns,output_memory_bytes\n".to_string();
    for m in values {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            m.experiment,
            m.variant,
            m.iteration,
            m.rows_input,
            m.rows_output,
            m.end_to_end_ns,
            m.execution_ns,
            m.output_memory_bytes
        ));
    }
    fs::write(path, csv)?;
    Ok(())
}
fn render_svg(summary: &BTreeMap<String, BTreeMap<String, u128>>) -> String {
    let width = 1100;
    let row_height = 34;
    let height = 80 + summary.values().map(BTreeMap::len).sum::<usize>() * row_height;
    let max = summary
        .values()
        .flat_map(|v| v.values())
        .copied()
        .max()
        .unwrap_or(1) as f64;
    let mut svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' viewBox='0 0 {width} {height}'><style>text{{font:14px sans-serif}} .title{{font:bold 20px sans-serif}}</style><rect width='100%' height='100%' fill='white'/><text x='20' y='30' class='title'>Lamina SQL benchmark medians (lower is better)</text>"
    );
    let mut y = 65;
    for (experiment, variants) in summary {
        for (variant, ns) in variants {
            let bar = (*ns as f64 / max * 650.0).max(1.0);
            svg.push_str(&format!("<text x='20' y='{}'>{} / {}</text><rect x='340' y='{}' width='{:.1}' height='20' fill='#4f46e5'/><text x='{:.1}' y='{}'>{:.3} ms</text>", y+15, experiment, variant, y, bar, 350.0+bar, y+15, *ns as f64/1_000_000.0));
            y += row_height;
        }
    }
    svg.push_str("</svg>");
    svg
}
