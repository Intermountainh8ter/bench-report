# bench-report

Shared `make_report` for the pre-registered bench suite. Each crate's
pre-registration says "Results live in `results/*.json`; README numbers
are rendered from committed JSON by `scripts/make_report`" — this is that
renderer.

## Usage

```
bench-report list --dir <crate>/results/
bench-report render --dir <crate>/results/ --fields verdict,asr,ci_lo,ci_hi
bench-report render --dir <crate>/results/ --fields verdict,n --splice-into <crate>/README.md
```

List prints every result file's top-level keys so you can decide which
fields to render. Render emits a Markdown table — to stdout by default,
or spliced into a README between the sentinel comments
`<!-- RESULTS:START -->` and `<!-- RESULTS:END -->`.

## Why Rust

Every tool in this suite is Rust. A shell `make_report` would have
diverged by now; a shared Rust tool stays consistent across every
crate's result shape.

## What this does NOT do

- Compute metrics. The result files are produced by each crate's own
  binary; this tool just renders them.
- Validate pre-reg clauses. That is the crate's own verdict field;
  this tool prints whatever the file says.
- Silent n/a. A render that is missing any requested field exits 1
  with the missing path named — a reviewer sees drift, not a blank cell.

## License

MIT.
