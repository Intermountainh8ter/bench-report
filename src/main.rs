//! bench-report: a Rust implementation of the shared `make_report`.
//!
//! Each bench crate's pre-registration says "Results live in
//! `results/*.json`" and "README numbers are rendered from committed
//! JSON by scripts/make_report." This tool is that renderer. It reads
//! a directory of result JSONs, extracts a configurable set of
//! top-level fields (string or number), and emits a Markdown table —
//! either to stdout or spliced into a README between the sentinel
//! comments:
//!
//! ```text
//! <!-- RESULTS:START -->
//! ...table rendered here...
//! <!-- RESULTS:END -->
//! ```
//!
//! Per-crate result shapes differ, so the field list is a CLI argument.
//! The tool refuses to render if any requested field is missing from a
//! file — a silent "n/a" would hide drift.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "bench-report", version, about = "Shared results-to-README renderer")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the result files and their top-level fields. Useful when
    /// deciding which fields to pass to `render`.
    List {
        /// Results directory. All `*.json` files are read.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Render a Markdown table of the requested fields from every JSON
    /// in the results directory. Exit code 1 if any requested field is
    /// missing from any file.
    Render {
        #[arg(long)]
        dir: PathBuf,
        /// Comma-separated list of top-level JSON field names to render
        /// as columns. The first column is always the filename stem.
        #[arg(long)]
        fields: String,
        /// If set, splice the table between `<!-- RESULTS:START -->`
        /// and `<!-- RESULTS:END -->` in this README. Fails if neither
        /// sentinel is present.
        #[arg(long)]
        splice_into: Option<PathBuf>,
    },
    /// Write a reproducibility manifest: SHA-256 of every regular file
    /// under each given path (recursively; hidden files skipped), plus the
    /// toolchain and the time. Exit code 1 if `--check` is given and any
    /// hash in the existing manifest differs.
    Manifest {
        /// Files or directories to hash. Directories are walked.
        #[arg(long, required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Destination manifest JSON.
        #[arg(long)]
        out: PathBuf,
        /// Compare against the existing manifest at `--out` instead of writing.
        #[arg(long, default_value_t = false)]
        check: bool,
    },
}

fn walk(p: &PathBuf, acc: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if p.is_dir() {
        let mut kids: Vec<PathBuf> = std::fs::read_dir(p)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|k| {
                !k.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
            })
            .collect();
        kids.sort();
        for k in kids {
            walk(&k, acc)?;
        }
    } else if p.is_file() {
        acc.push(p.clone());
    }
    Ok(())
}

fn sha256_file(p: &PathBuf) -> anyhow::Result<String> {
    use sha2::Digest;
    let bytes = std::fs::read(p)?;
    Ok(hex::encode(sha2::Sha256::digest(&bytes)))
}

fn build_manifest(paths: &[PathBuf]) -> anyhow::Result<serde_json::Value> {
    let mut files = Vec::new();
    for p in paths {
        walk(p, &mut files)?;
    }
    let mut entries = serde_json::Map::new();
    for f in &files {
        entries.insert(f.to_string_lossy().into_owned(), serde_json::Value::String(sha256_file(f)?));
    }
    let toolchain = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(serde_json::json!({
        "tool": format!("bench-report {}", env!("CARGO_PKG_VERSION")),
        "rustc": toolchain,
        "unix_time": now,
        "n_files": entries.len(),
        "sha256": entries,
    }))
}

fn read_results(dir: &PathBuf) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for p in entries {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
        out.push((stem, v));
    }
    Ok(out)
}

fn render_cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn render_table(
    results: &[(String, serde_json::Value)],
    fields: &[String],
) -> anyhow::Result<String> {
    let mut out = String::new();
    out.push('|');
    out.push_str(" file |");
    for f in fields {
        out.push(' ');
        out.push_str(f);
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    out.push_str("---|");
    for _ in fields {
        out.push_str("---|");
    }
    out.push('\n');
    for (stem, body) in results {
        out.push_str("| ");
        out.push_str(stem);
        out.push_str(" |");
        for f in fields {
            let v = body.get(f).ok_or_else(|| {
                anyhow::anyhow!(
                    "result `{}` is missing field `{}` — refuse to render a silent n/a",
                    stem,
                    f
                )
            })?;
            out.push(' ');
            out.push_str(&render_cell(v));
            out.push_str(" |");
        }
        out.push('\n');
    }
    Ok(out)
}

fn splice(readme: &PathBuf, table: &str) -> anyhow::Result<()> {
    const START: &str = "<!-- RESULTS:START -->";
    const END: &str = "<!-- RESULTS:END -->";
    let text = std::fs::read_to_string(readme)?;
    let start = text
        .find(START)
        .ok_or_else(|| anyhow::anyhow!("README is missing {START}"))?;
    let end = text
        .find(END)
        .ok_or_else(|| anyhow::anyhow!("README is missing {END}"))?;
    if end < start {
        anyhow::bail!("README has END before START");
    }
    let before = &text[..start + START.len()];
    let after = &text[end..];
    let mut spliced = String::with_capacity(text.len() + table.len());
    spliced.push_str(before);
    spliced.push('\n');
    spliced.push_str(table);
    spliced.push_str(after);
    std::fs::write(readme, spliced)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::List { dir } => {
            let rows = read_results(&dir)?;
            for (stem, body) in &rows {
                if let Some(obj) = body.as_object() {
                    let mut keys: Vec<&String> = obj.keys().collect();
                    keys.sort();
                    let joined = keys
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!("{stem}: {joined}");
                } else {
                    println!("{stem}: <non-object>");
                }
            }
        }
        Cmd::Render {
            dir,
            fields,
            splice_into,
        } => {
            let fields: Vec<String> = fields
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if fields.is_empty() {
                anyhow::bail!("--fields must list at least one field");
            }
            let rows = read_results(&dir)?;
            let table = render_table(&rows, &fields)?;
            match splice_into {
                None => print!("{table}"),
                Some(p) => splice(&p, &table)?,
            }
        }
        Cmd::Manifest { paths, out, check } => {
            let fresh = build_manifest(&paths)?;
            if check {
                let old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&out)?)?;
                let empty = serde_json::Map::new();
                let old_map = old.get("sha256").and_then(|v| v.as_object()).unwrap_or(&empty);
                let new_map = fresh.get("sha256").and_then(|v| v.as_object()).unwrap_or(&empty);
                let mut diffs = Vec::new();
                for (k, v) in old_map {
                    match new_map.get(k) {
                        None => diffs.push(format!("missing: {k}")),
                        Some(nv) if nv != v => diffs.push(format!("changed: {k}")),
                        _ => {}
                    }
                }
                for k in new_map.keys() {
                    if !old_map.contains_key(k) {
                        diffs.push(format!("new: {k}"));
                    }
                }
                if diffs.is_empty() {
                    println!("manifest OK: {} files match", old_map.len());
                } else {
                    for d in &diffs {
                        println!("{d}");
                    }
                    std::process::exit(1);
                }
            } else {
                std::fs::write(&out, serde_json::to_string_pretty(&fresh)?)?;
                println!(
                    "wrote {} ({} files)",
                    out.display(),
                    fresh.get("n_files").and_then(|n| n.as_u64()).unwrap_or(0)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_table_prints_filename_then_requested_fields() {
        let rows = vec![
            ("a".into(), json!({"x": 1, "y": "yes"})),
            ("b".into(), json!({"x": 2, "y": "no"})),
        ];
        let table = render_table(&rows, &["x".into(), "y".into()]).expect("ok");
        assert!(table.contains("| file | x | y |"));
        assert!(table.contains("| a | 1 | yes |"));
        assert!(table.contains("| b | 2 | no |"));
    }

    #[test]
    fn render_table_fails_loudly_when_field_missing() {
        let rows = vec![("a".into(), json!({"x": 1}))];
        let err = render_table(&rows, &["x".into(), "y".into()]).expect_err("y missing");
        assert!(err.to_string().contains("missing field `y`"));
    }

    #[test]
    fn splice_rejects_a_readme_with_no_sentinels() {
        let dir = tempdir();
        let path = dir.join("README.md");
        std::fs::write(&path, "no sentinels here").unwrap();
        let err = splice(&path, "whatever").expect_err("no sentinels");
        assert!(err.to_string().contains("RESULTS:START"));
    }

    #[test]
    fn splice_replaces_content_between_sentinels() {
        let dir = tempdir();
        let path = dir.join("README.md");
        std::fs::write(
            &path,
            "before\n<!-- RESULTS:START -->\nold stuff\n<!-- RESULTS:END -->\nafter\n",
        )
        .unwrap();
        splice(&path, "NEW\n").expect("ok");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("NEW"));
        assert!(!after.contains("old stuff"));
        assert!(after.contains("before"));
        assert!(after.contains("after"));
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!(
            "bench-report-test-{}-{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
