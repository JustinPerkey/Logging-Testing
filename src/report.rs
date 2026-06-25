//! Turning a list of [`CaseResult`]s into CSV, JSON, a console table and a
//! plain-language recommendation.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::metrics::{fmt_ns, fmt_rate, CaseResult};

/// Write all results as a CSV file (one row per case).
pub fn write_csv(results: &[CaseResult], path: &Path) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    writeln!(
        f,
        "strategy,producers,messages_per_producer,total_messages,msg_size,capacity,\
writer_buf_bytes,full_policy,target_rate_per_producer,dropped,enqueue_secs,drain_secs,\
enqueue_throughput,end_to_end_throughput,mb_per_sec,lat_mean_ns,lat_min_ns,lat_p50_ns,\
lat_p90_ns,lat_p99_ns,lat_p999_ns,lat_max_ns"
    )?;
    for r in results {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{:.6},{:.6},{:.2},{:.2},{:.4},{:.1},{},{},{},{},{},{}",
            r.strategy.name(),
            r.producers,
            r.messages_per_producer,
            r.total_messages,
            r.msg_size,
            r.capacity,
            r.writer_buf_bytes,
            r.full_policy,
            r.target_rate_per_producer
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "unbounded".into()),
            r.dropped,
            r.enqueue_secs,
            r.drain_secs,
            r.enqueue_throughput,
            r.end_to_end_throughput,
            r.mb_per_sec,
            r.latency.mean_ns,
            r.latency.min_ns,
            r.latency.p50_ns,
            r.latency.p90_ns,
            r.latency.p99_ns,
            r.latency.p999_ns,
            r.latency.max_ns,
        )?;
    }
    Ok(())
}

/// Write all results as pretty-printed JSON.
pub fn write_json(results: &[CaseResult], path: &Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(results).expect("results are serializable");
    std::fs::write(path, json)
}

/// Print a human-readable table of every case to stdout.
pub fn print_table(results: &[CaseResult]) {
    // Column headers and the closures that render each cell.
    let headers = [
        "strategy", "size", "cap", "prod", "rate", "p50", "p99", "p99.9", "max", "thrpt", "MB/s",
        "drop",
    ];
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            vec![
                r.strategy.name().to_string(),
                format!("{}B", r.msg_size),
                if r.capacity == 0 {
                    "∞".to_string()
                } else {
                    r.capacity.to_string()
                },
                r.producers.to_string(),
                r.target_rate_per_producer
                    .map(fmt_rate)
                    .unwrap_or_else(|| "max".into()),
                fmt_ns(r.latency.p50_ns as f64),
                fmt_ns(r.latency.p99_ns as f64),
                fmt_ns(r.latency.p999_ns as f64),
                fmt_ns(r.latency.max_ns as f64),
                fmt_rate(r.end_to_end_throughput),
                format!("{:.1}", r.mb_per_sec),
                r.dropped.to_string(),
            ]
        })
        .collect();

    // Compute column widths.
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let sep = |widths: &[usize]| {
        let mut s = String::from("+");
        for w in widths {
            s.push_str(&"-".repeat(w + 2));
            s.push('+');
        }
        s
    };

    let render = |row: &[String], widths: &[usize]| {
        let mut s = String::from("|");
        for (i, cell) in row.iter().enumerate() {
            let pad = widths[i] - cell.chars().count();
            s.push(' ');
            s.push_str(cell);
            s.push_str(&" ".repeat(pad));
            s.push_str(" |");
        }
        s
    };

    println!("{}", sep(&widths));
    println!(
        "{}",
        render(
            &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
            &widths
        )
    );
    println!("{}", sep(&widths));
    for row in &rows {
        println!("{}", render(row, &widths));
    }
    println!("{}", sep(&widths));
}

/// Key grouping comparable cases: same message size, capacity, producers and rate.
fn group_key(r: &CaseResult) -> (usize, usize, usize, u64) {
    let rate_bits = r.target_rate_per_producer.map(|v| v.to_bits()).unwrap_or(0);
    (r.msg_size, r.capacity, r.producers, rate_bits)
}

/// Print a plain-language recommendation derived from the results.
///
/// For every comparable group we highlight:
/// * the **lowest p99 hot-path latency** among strategies that dropped nothing
///   (the best choice when you must not lose log lines), and
/// * the **highest end-to-end throughput** overall.
pub fn print_recommendations(results: &[CaseResult]) {
    if results.is_empty() {
        return;
    }

    let mut groups: BTreeMap<(usize, usize, usize, u64), Vec<&CaseResult>> = BTreeMap::new();
    for r in results {
        groups.entry(group_key(r)).or_default().push(r);
    }

    println!("\n=== Recommendations ===");
    println!(
        "For each workload (size / buffer / producers / rate) the lowest-tail-latency\n\
         lossless strategy and the highest-throughput strategy on THIS machine:\n"
    );

    for (_, group) in groups {
        let sample = group[0];
        let rate = sample
            .target_rate_per_producer
            .map(fmt_rate)
            .unwrap_or_else(|| "max".into());

        // Lowest p99 among lossless (zero-drop) runs.
        let best_latency = group
            .iter()
            .filter(|r| r.dropped == 0)
            .min_by_key(|r| r.latency.p99_ns);
        // Highest end-to-end throughput overall.
        let best_thrpt = group
            .iter()
            .max_by(|a, b| {
                a.end_to_end_throughput
                    .partial_cmp(&b.end_to_end_throughput)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();

        println!(
            "• {size}B, cap {cap}, {prod} producer(s), {rate} rate:",
            size = sample.msg_size,
            cap = if sample.capacity == 0 {
                "∞".to_string()
            } else {
                sample.capacity.to_string()
            },
            prod = sample.producers,
            rate = rate,
        );
        match best_latency {
            Some(r) => println!(
                "    lowest tail latency (lossless): {:<16} p99={}",
                r.strategy.name(),
                fmt_ns(r.latency.p99_ns as f64)
            ),
            None => {
                println!("    lowest tail latency (lossless): (every strategy dropped records)")
            }
        }
        println!(
            "    highest throughput:             {:<16} {}{}",
            best_thrpt.strategy.name(),
            fmt_rate(best_thrpt.end_to_end_throughput),
            if best_thrpt.dropped > 0 {
                format!(" (dropped {})", best_thrpt.dropped)
            } else {
                String::new()
            },
        );
    }

    // A single headline pick: best lossless p99 across the whole sweep.
    if let Some(overall) = results
        .iter()
        .filter(|r| r.dropped == 0)
        .min_by_key(|r| r.latency.p99_ns)
    {
        println!(
            "\nHeadline: across this sweep, '{}' gave the best lossless tail latency \
             (p99={} at {}B / cap {} / {} producers).",
            overall.strategy.name(),
            fmt_ns(overall.latency.p99_ns as f64),
            overall.msg_size,
            if overall.capacity == 0 {
                "∞".to_string()
            } else {
                overall.capacity.to_string()
            },
            overall.producers,
        );
    }
    println!(
        "\nNote: 'best' depends on what you value. Direct/blocking often wins raw\n\
         throughput on fast disks, but an async strategy keeps the hot path's tail\n\
         latency low and isolates producers from I/O stalls. Re-run with your real\n\
         message sizes and rates for a decision that reflects your workload."
    );
}
