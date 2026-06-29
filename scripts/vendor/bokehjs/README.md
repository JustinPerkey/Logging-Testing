# Vendored BokehJS

These minified BokehJS bundles are inlined verbatim into `plots.html` by
`scripts/aggregate.py`, so the generated report is **fully self-contained and
works offline** (e.g. on an air-gapped device under test) — no CDN fetch at view
time.

- `bokeh-3.4.1.min.js` — BokehJS core.
- `bokeh-api-3.4.1.min.js` — the `Bokeh.Plotting` convenience API the report uses.

Source: <https://cdn.bokeh.org/bokeh/release/> (version **3.4.1**), identical to
the files shipped inside the `bokeh` PyPI package under
`bokeh/server/static/js/`. They carry Bokeh's BSD 3-Clause license in their own
header comments (Copyright Anaconda, Inc. and Bokeh Contributors).

To upgrade: drop the new `bokeh-<ver>.min.js` / `bokeh-api-<ver>.min.js` here and
bump `BOKEHJS_VERSION` in `scripts/aggregate.py`. If a vendored file is missing,
`aggregate.py` falls back to loading that version from the Bokeh CDN.
