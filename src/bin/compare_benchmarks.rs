use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

type Summary = BTreeMap<String, BTreeMap<String, u128>>;

#[derive(Debug, Serialize)]
struct Comparison {
    experiment: String,
    variant: String,
    baseline_ns: u128,
    candidate_ns: u128,
    speedup: f64,
    latency_change_percent: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("comparison failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let baseline_path = required(arguments.next(), "baseline summary.json")?;
    let candidate_path = required(arguments.next(), "candidate summary.json")?;
    let output_directory = PathBuf::from(required(arguments.next(), "output directory")?);
    if arguments.next().is_some() {
        return Err(
            "usage: compare_benchmarks <baseline.json> <candidate.json> <output-dir>".into(),
        );
    }
    let baseline: Summary = serde_json::from_str(&fs::read_to_string(&baseline_path)?)?;
    let candidate: Summary = serde_json::from_str(&fs::read_to_string(&candidate_path)?)?;
    let comparisons = compare(&baseline, &candidate);
    fs::create_dir_all(&output_directory)?;
    fs::write(
        output_directory.join("comparison.json"),
        serde_json::to_string_pretty(&comparisons)?,
    )?;
    fs::write(
        output_directory.join("comparison.md"),
        render_markdown(&comparisons, &baseline_path, &candidate_path),
    )?;
    fs::write(
        output_directory.join("comparison.svg"),
        render_svg(&comparisons),
    )?;
    println!(
        "wrote {} matched comparisons to {}",
        comparisons.len(),
        output_directory.display()
    );
    Ok(())
}

fn required(value: Option<String>, description: &str) -> Result<String, String> {
    value.ok_or_else(|| format!("missing {description}"))
}

fn compare(baseline: &Summary, candidate: &Summary) -> Vec<Comparison> {
    let mut output = Vec::new();
    for (experiment, variants) in baseline {
        for (variant, baseline_ns) in variants {
            let Some(candidate_ns) = candidate
                .get(experiment)
                .and_then(|values| values.get(variant))
            else {
                continue;
            };
            output.push(Comparison {
                experiment: experiment.clone(),
                variant: variant.clone(),
                baseline_ns: *baseline_ns,
                candidate_ns: *candidate_ns,
                speedup: *baseline_ns as f64 / *candidate_ns as f64,
                latency_change_percent: (*candidate_ns as f64 / *baseline_ns as f64 - 1.0) * 100.0,
            });
        }
    }
    output
}

fn render_markdown(values: &[Comparison], baseline: &str, candidate: &str) -> String {
    let mut output = format!(
        "# Before/after benchmark comparison\n\nBaseline: `{baseline}`  \nCandidate: `{candidate}`\n\n| Experiment | Variant | Baseline | Candidate | Speedup | Latency change |\n|---|---|---:|---:|---:|---:|\n"
    );
    for value in values {
        output.push_str(&format!(
            "| {} | {} | {:.3} ms | {:.3} ms | {:.2}× | {:+.1}% |\n",
            value.experiment,
            value.variant,
            value.baseline_ns as f64 / 1_000_000.0,
            value.candidate_ns as f64 / 1_000_000.0,
            value.speedup,
            value.latency_change_percent,
        ));
    }
    output.push_str("\nNegative latency change is an improvement. Medians are compared only when experiment and variant names match.\n");
    output
}

fn render_svg(values: &[Comparison]) -> String {
    let width = 1100;
    let row_height = 34;
    let height = 70 + values.len() * row_height;
    let max_speedup = values
        .iter()
        .map(|value| value.speedup)
        .fold(1.0f64, f64::max);
    let mut svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' viewBox='0 0 {width} {height}'><style>text{{font:14px sans-serif}} .title{{font:bold 20px sans-serif}}</style><rect width='100%' height='100%' fill='white'/><text x='20' y='30' class='title'>Typed execution refactor: median speedup by benchmark case</text>"
    );
    let mut y = 55;
    for value in values {
        let bar = (value.speedup / max_speedup * 560.0).max(2.0);
        let color = if value.speedup >= 1.0 {
            "#0f766e"
        } else {
            "#dc2626"
        };
        svg.push_str(&format!(
            "<text x='20' y='{}'>{} / {}</text><rect x='350' y='{}' width='{:.1}' height='20' fill='{color}'/><text x='{:.1}' y='{}'>{:.2}×</text>",
            y + 15,
            escape_xml(&value.experiment),
            escape_xml(&value.variant),
            y,
            bar,
            360.0 + bar,
            y + 15,
            value.speedup,
        ));
        y += row_height;
    }
    svg.push_str("</svg>");
    svg
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_only_matching_cases() {
        let baseline =
            BTreeMap::from([("filter".into(), BTreeMap::from([("on".into(), 200u128)]))]);
        let candidate = BTreeMap::from([(
            "filter".into(),
            BTreeMap::from([("on".into(), 100u128), ("new".into(), 50)]),
        )]);
        let values = compare(&baseline, &candidate);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].speedup, 2.0);
        assert_eq!(values[0].latency_change_percent, -50.0);
    }
}
