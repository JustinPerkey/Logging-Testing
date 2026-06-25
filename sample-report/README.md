# Sample report

`REPORT.md` here is a **real, illustrative** run produced by the overnight
harness — but a deliberately *short* one (`SMOKE=1`, 12 trials, a reduced sweep),
generated on a shared 4-core CI VM. It exists so you can see the shape of the
output without waiting for a full overnight run.

Treat the absolute numbers as **noisy** (note the high coefficients of variation
on a shared VM); the point is the structure and the relative ordering. For
decision-quality numbers, run the full harness on your own hardware:

```bash
scripts/overnight.sh            # ~hours; writes overnight-out/REPORT.md
```

Files:

- `REPORT.md` — the generated report (Markdown).
- `summary_stats.csv` — every (strategy, workload, metric) aggregate with
  mean / 95% CI / stdev / CV / min / median / max.
- `run_meta.json` — the captured run environment + parameters.
