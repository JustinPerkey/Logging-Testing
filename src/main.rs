//! `logbench` — benchmark asynchronous, non-blocking logging strategies and
//! find the best one for your machine and workload.

use std::path::PathBuf;

use clap::Parser;

use logbench::config::{FullPolicy, LoggerConfig, Strategy, Workload};
use logbench::report;
use logbench::runner::run_case;

/// Benchmark async/non-blocking logging strategies across a sweep of message
/// sizes, buffer capacities, producer counts and log rates.
#[derive(Parser, Debug)]
#[command(name = "logbench", version, about, long_about = None)]
struct Cli {
    /// Strategies to test (comma-separated), or "all".
    /// Options: direct, crossbeam, flume, tracing-appender.
    #[arg(long, default_value = "all", value_delimiter = ',')]
    strategies: Vec<String>,

    /// Message sizes to sweep, in bytes (comma-separated).
    #[arg(long, default_value = "64,512,4096", value_delimiter = ',')]
    msg_sizes: Vec<usize>,

    /// Buffer capacities to sweep, in records (0 = unbounded channel).
    /// For the `direct` strategy this knob is ignored.
    #[arg(long, default_value = "8192", value_delimiter = ',')]
    buffers: Vec<usize>,

    /// Producer-thread counts to sweep (comma-separated).
    #[arg(long, default_value = "4", value_delimiter = ',')]
    producers: Vec<usize>,

    /// Target rates **per producer** in records/second (0 = max throughput).
    #[arg(long, default_value = "0", value_delimiter = ',')]
    rates: Vec<f64>,

    /// Measured records emitted per producer in each case.
    #[arg(long, default_value_t = 200_000)]
    messages: u64,

    /// Untimed warmup records per producer before measurement.
    #[arg(long, default_value_t = 5_000)]
    warmup: u64,

    /// Bytes for the background `BufWriter` fronting the file.
    #[arg(long, default_value_t = 64 * 1024)]
    writer_buf: usize,

    /// Behaviour when a bounded buffer is full: block (lossless) or drop (lossy).
    #[arg(long, default_value = "block")]
    full_policy: String,

    /// Directory for log output files and result files.
    #[arg(long, default_value = "bench-out")]
    out_dir: PathBuf,

    /// Keep the generated log files instead of deleting them after each case.
    #[arg(long, default_value_t = false)]
    keep_logs: bool,

    /// Write a CSV of results to this path (defaults to <out-dir>/results.csv).
    #[arg(long)]
    csv: Option<PathBuf>,

    /// Write a JSON array of results to this path (defaults to <out-dir>/results.json).
    #[arg(long)]
    json: Option<PathBuf>,
}

fn parse_strategies(raw: &[String]) -> Result<Vec<Strategy>, String> {
    if raw.iter().any(|s| s.trim().eq_ignore_ascii_case("all")) {
        return Ok(Strategy::ALL.to_vec());
    }
    raw.iter().map(|s| s.parse::<Strategy>()).collect()
}

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    let strategies = match parse_strategies(&cli.strategies) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let full_policy: FullPolicy = match cli.full_policy.parse() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    std::fs::create_dir_all(&cli.out_dir)?;

    // Build the full sweep matrix.
    let total_cases = strategies.len()
        * cli.msg_sizes.len()
        * cli.buffers.len()
        * cli.producers.len()
        * cli.rates.len();
    println!(
        "logbench: {} case(s) — strategies={:?}, sizes={:?}B, buffers={:?}, producers={:?}, rates={:?}",
        total_cases,
        strategies.iter().map(|s| s.name()).collect::<Vec<_>>(),
        cli.msg_sizes,
        cli.buffers,
        cli.producers,
        cli.rates,
    );
    println!(
        "  {} measured msgs/case (+{} warmup) per producer, writer_buf={}B, full_policy={}\n",
        cli.messages, cli.warmup, cli.writer_buf, full_policy
    );

    let mut results = Vec::with_capacity(total_cases);
    let mut case_no = 0usize;

    for &size in &cli.msg_sizes {
        for &cap in &cli.buffers {
            for &producers in &cli.producers {
                for &rate in &cli.rates {
                    for &strategy in &strategies {
                        case_no += 1;
                        let target_rate = if rate > 0.0 { Some(rate) } else { None };
                        let workload = Workload {
                            producers,
                            messages_per_producer: cli.messages,
                            msg_size: size,
                            target_rate_per_producer: target_rate,
                            warmup: cli.warmup,
                        };
                        let log_path = cli.out_dir.join(format!(
                            "{}_s{}_c{}_p{}_r{}.log",
                            strategy.name(),
                            size,
                            cap,
                            producers,
                            rate as u64,
                        ));
                        let cfg = LoggerConfig {
                            path: log_path.clone(),
                            capacity: cap,
                            writer_buf_bytes: cli.writer_buf,
                            full_policy,
                        };

                        print!(
                            "[{case_no}/{total_cases}] {:<16} size={size:<5} cap={cap:<6} \
                             producers={producers} rate={} ... ",
                            strategy.name(),
                            if rate > 0.0 {
                                format!("{rate:.0}")
                            } else {
                                "max".into()
                            }
                        );
                        use std::io::Write as _;
                        std::io::stdout().flush().ok();

                        let result = run_case(strategy, &cfg, workload)?;
                        println!(
                            "p99={} thrpt={} drop={}",
                            logbench::metrics::fmt_ns(result.latency.p99_ns as f64),
                            logbench::metrics::fmt_rate(result.end_to_end_throughput),
                            result.dropped,
                        );
                        results.push(result);

                        if !cli.keep_logs {
                            let _ = std::fs::remove_file(&log_path);
                        }
                    }
                }
            }
        }
    }

    println!();
    report::print_table(&results);
    report::print_recommendations(&results);

    let csv_path = cli.csv.unwrap_or_else(|| cli.out_dir.join("results.csv"));
    let json_path = cli.json.unwrap_or_else(|| cli.out_dir.join("results.json"));
    report::write_csv(&results, &csv_path)?;
    report::write_json(&results, &json_path)?;
    println!(
        "\nWrote {} results to {} and {}",
        results.len(),
        csv_path.display(),
        json_path.display()
    );

    Ok(())
}
