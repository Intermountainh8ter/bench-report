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
    /// Render one HTML status page across many repositories from a JSON
    /// config. Every number on the page comes from a committed result file
    /// or a README block that a repo's own renderer produced; the page adds
    /// git provenance (commit, date, dirty flag) per repo and nothing else.
    Dashboard {
        /// Config JSON: {"title","projects":[{name,family,dir,repo_url,note,sources:[...]}]}.
        #[arg(long)]
        config: PathBuf,
        /// Directory that relative `dir` entries in the config resolve against.
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        out: PathBuf,
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

// ---------- dashboard ----------

mod dashboard {
    use serde::Deserialize;
    use std::path::{Path, PathBuf};

    #[derive(Deserialize)]
    pub struct Config {
        pub title: String,
        #[serde(default)]
        pub subtitle: String,
        pub projects: Vec<Project>,
    }

    #[derive(Deserialize)]
    pub struct Project {
        pub name: String,
        pub family: String,
        pub dir: String,
        #[serde(default)]
        pub repo_url: String,
        #[serde(default)]
        pub note: String,
        #[serde(default)]
        pub sources: Vec<Source>,
    }

    /// Where a project's numbers come from. Each variant reads committed files only.
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum Source {
        /// A block of the repo README between two sentinel comments, already rendered
        /// by that repo's own tool from its result files.
        ReadmeBlock { start: String, end: String, #[serde(default)] file: Option<String> },
        /// JSON files holding an `outcomes` array of "Success" / "Fail" / "Error".
        Outcomes { glob: String, #[serde(default)] heading: String },
        /// JSON files; one row per file, one column per JSON pointer.
        Fields { glob: String, columns: Vec<Column>, #[serde(default)] heading: String },
    }

    #[derive(Deserialize)]
    pub struct Column {
        pub label: String,
        pub pointer: String,
        #[serde(default)]
        pub percent: bool,
    }

    pub struct Provenance {
        pub commit: String,
        pub date: String,
        pub dirty: bool,
    }

    pub fn provenance(dir: &Path) -> Provenance {
        let run = |args: &[&str]| -> String {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        let commit = run(&["rev-parse", "--short", "HEAD"]);
        let date = run(&["log", "-1", "--format=%cd", "--date=short"]);
        let dirty = !run(&["status", "--porcelain", "--untracked-files=no"]).is_empty();
        Provenance { commit, date, dirty }
    }

    pub fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    /// Inline markdown: code spans, links, bold. Everything else is escaped text.
    pub fn inline(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while !rest.is_empty() {
            if let Some(i) = rest.find('`') {
                out.push_str(&inline_links(&rest[..i]));
                let after = &rest[i + 1..];
                if let Some(j) = after.find('`') {
                    out.push_str("<code>");
                    out.push_str(&esc(&after[..j]));
                    out.push_str("</code>");
                    rest = &after[j + 1..];
                } else {
                    out.push_str(&esc(&rest[i..]));
                    break;
                }
            } else {
                out.push_str(&inline_links(rest));
                break;
            }
        }
        out
    }

    fn inline_links(s: &str) -> String {
        // [text](url) and **bold**, applied to escaped text.
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('[') {
            let Some(close) = rest[i..].find("](") else { break };
            let Some(end) = rest[i + close..].find(')') else { break };
            let text = &rest[i + 1..i + close];
            let url = &rest[i + close + 2..i + close + end];
            out.push_str(&bold(&esc(&rest[..i])));
            if url.starts_with("http") {
                out.push_str(&format!("<a href=\"{}\">{}</a>", esc(url), bold(&esc(text))));
            } else {
                out.push_str(&bold(&esc(text)));
            }
            rest = &rest[i + close + end + 1..];
        }
        out.push_str(&bold(&esc(rest)));
        out
    }

    fn bold(s: &str) -> String {
        let mut out = String::new();
        let mut parts = s.split("**");
        if let Some(first) = parts.next() {
            out.push_str(first);
        }
        let mut open = true;
        for p in parts {
            out.push_str(if open { "<strong>" } else { "</strong>" });
            out.push_str(p);
            open = !open;
        }
        if !open {
            out.push_str("</strong>");
        }
        out
    }

    /// Markdown subset to HTML: pipe tables, bullet lists, paragraphs, `##`/`###` headings.
    pub fn markdown(md: &str) -> String {
        let mut html = String::new();
        let lines: Vec<&str> = md.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i].trim_end();
            if l.trim().is_empty() {
                i += 1;
                continue;
            }
            if l.starts_with('|') {
                let mut rows: Vec<Vec<String>> = Vec::new();
                while i < lines.len() && lines[i].trim_start().starts_with('|') {
                    let cells: Vec<String> = lines[i].trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
                    if !cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ') && !c.is_empty()) {
                        rows.push(cells);
                    }
                    i += 1;
                }
                html.push_str("<div class=\"tbl\"><table>");
                for (r, cells) in rows.iter().enumerate() {
                    html.push_str("<tr>");
                    for c in cells {
                        let tag = if r == 0 { "th" } else { "td" };
                        html.push_str(&format!("<{tag}>{}</{tag}>", inline(c)));
                    }
                    html.push_str("</tr>");
                }
                html.push_str("</table></div>");
                continue;
            }
            if l.starts_with("- ") || l.starts_with("* ") {
                html.push_str("<ul>");
                while i < lines.len() && (lines[i].starts_with("- ") || lines[i].starts_with("* ")) {
                    html.push_str(&format!("<li>{}</li>", inline(&lines[i][2..])));
                    i += 1;
                }
                html.push_str("</ul>");
                continue;
            }
            if let Some(h) = l.strip_prefix("### ") {
                html.push_str(&format!("<h4>{}</h4>", inline(h)));
                i += 1;
                continue;
            }
            if let Some(h) = l.strip_prefix("## ") {
                html.push_str(&format!("<h4>{}</h4>", inline(h)));
                i += 1;
                continue;
            }
            let mut para = String::new();
            while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].starts_with('|') && !lines[i].starts_with("- ") && !lines[i].starts_with('#') {
                if !para.is_empty() {
                    para.push(' ');
                }
                para.push_str(lines[i].trim());
                i += 1;
            }
            html.push_str(&format!("<p>{}</p>", inline(&para)));
        }
        html
    }

    pub fn readme_block(dir: &Path, file: Option<&str>, start: &str, end: &str) -> anyhow::Result<String> {
        let path = dir.join(file.unwrap_or("README.md"));
        let text = std::fs::read_to_string(&path)?;
        let s = text.find(start).ok_or_else(|| anyhow::anyhow!("{} has no `{start}`", path.display()))?;
        let e = text[s..].find(end).ok_or_else(|| anyhow::anyhow!("{} has no `{end}`", path.display()))? + s;
        Ok(markdown(&text[s + start.len()..e]))
    }

    fn glob_files(dir: &Path, pattern: &str) -> anyhow::Result<Vec<PathBuf>> {
        // Supports `<subdir>/<prefix>*<suffix>` with a single `*` in the final component.
        let (sub, pat) = match pattern.rsplit_once('/') {
            Some((a, b)) => (dir.join(a), b.to_string()),
            None => (dir.to_path_buf(), pattern.to_string()),
        };
        let (prefix, suffix) = pat.split_once('*').unwrap_or((pat.as_str(), ""));
        let mut out: Vec<PathBuf> = match std::fs::read_dir(&sub) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_file()
                        && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix) && n.len() >= prefix.len() + suffix.len())
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        out.sort();
        Ok(out)
    }

    pub fn outcomes(dir: &Path, pattern: &str, heading: &str) -> anyhow::Result<String> {
        let files = glob_files(dir, pattern)?;
        if files.is_empty() {
            return Ok(format!("<p class=\"muted\">No result files yet ({}).</p>", esc(pattern)));
        }
        let mut html = String::new();
        if !heading.is_empty() {
            html.push_str(&format!("<h4>{}</h4>", esc(heading)));
        }
        html.push_str("<div class=\"tbl\"><table><tr><th>result file</th><th class=\"n\">n</th><th class=\"n\">non-refusal</th><th class=\"n\">errors</th></tr>");
        for f in files {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f)?)?;
            let Some(arr) = v.get("outcomes").and_then(|a| a.as_array()) else { continue };
            let n = arr.len();
            let succ = arr.iter().filter(|o| o.as_str() == Some("Success")).count();
            let err = arr.iter().filter(|o| o.as_str() == Some("Error")).count();
            let denom = n.saturating_sub(err);
            let rate = if denom > 0 { succ as f64 / denom as f64 } else { f64::NAN };
            html.push_str(&format!(
                "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{:.1}%</td><td class=\"n\">{}</td></tr>",
                esc(f.file_stem().and_then(|s| s.to_str()).unwrap_or("?")),
                n,
                rate * 100.0,
                err
            ));
        }
        html.push_str("</table></div>");
        Ok(html)
    }

    fn pointer<'a>(v: &'a serde_json::Value, p: &str) -> Option<&'a serde_json::Value> {
        let mut cur = v;
        for k in p.trim_start_matches('/').split('/') {
            if k.is_empty() {
                continue;
            }
            cur = match cur {
                serde_json::Value::Object(m) => m.get(k)?,
                serde_json::Value::Array(a) => a.get(k.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn fields(dir: &Path, pattern: &str, cols: &[Column], heading: &str) -> anyhow::Result<String> {
        let files = glob_files(dir, pattern)?;
        if files.is_empty() {
            return Ok(format!("<p class=\"muted\">No result files yet ({}).</p>", esc(pattern)));
        }
        let mut html = String::new();
        if !heading.is_empty() {
            html.push_str(&format!("<h4>{}</h4>", esc(heading)));
        }
        html.push_str("<div class=\"tbl\"><table><tr><th>file</th>");
        for c in cols {
            html.push_str(&format!("<th>{}</th>", esc(&c.label)));
        }
        html.push_str("</tr>");
        for f in files {
            // Python-written files sometimes carry bare NaN / Infinity tokens, which are not JSON.
            // They are read as null (rendered "null"), never as a number.
            let text = std::fs::read_to_string(&f)?
                .replace(": NaN", ": null")
                .replace(": -Infinity", ": null")
                .replace(": Infinity", ": null");
            let v: serde_json::Value = serde_json::from_str(&text)?;
            html.push_str(&format!("<tr><td>{}</td>", esc(f.file_stem().and_then(|s| s.to_str()).unwrap_or("?"))));
            for c in cols {
                let cell = match pointer(&v, &c.pointer) {
                    None => "<span class=\"missing\">missing</span>".to_string(),
                    Some(serde_json::Value::Number(n)) => {
                        let x = n.as_f64().unwrap_or(f64::NAN);
                        if c.percent {
                            format!("{:.1}%", x * 100.0)
                        } else if x.fract() == 0.0 && x.abs() < 1e12 {
                            format!("{}", x as i64)
                        } else {
                            format!("{x:.3}")
                        }
                    }
                    Some(serde_json::Value::String(s)) => esc(s),
                    Some(serde_json::Value::Bool(b)) => b.to_string(),
                    Some(serde_json::Value::Null) => "null".into(),
                    Some(other) => esc(&other.to_string()),
                };
                html.push_str(&format!("<td>{cell}</td>"));
            }
            html.push_str("</tr>");
        }
        html.push_str("</table></div>");
        Ok(html)
    }

    pub fn newest_result(dir: &Path) -> (usize, String) {
        let mut n = 0usize;
        let mut newest: Option<std::time::SystemTime> = None;
        fn walk(p: &Path, n: &mut usize, newest: &mut Option<std::time::SystemTime>) {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        walk(&path, n, newest);
                    } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        *n += 1;
                        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                            if newest.map_or(true, |x| m > x) {
                                *newest = Some(m);
                            }
                        }
                    }
                }
            }
        }
        walk(&dir.join("results"), &mut n, &mut newest);
        let when = newest
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| fmt_utc(d.as_secs() as i64))
            .unwrap_or_else(|| "none".into());
        (n, when)
    }

    /// `YYYY-MM-DD HH:MM UTC` from unix seconds (civil-from-days, Howard Hinnant).
    pub fn fmt_utc(secs: i64) -> String {
        let days = secs.div_euclid(86400);
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        let rem = secs.rem_euclid(86400);
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
    }
}

fn render_dashboard(config: &PathBuf, base: &PathBuf, out: &PathBuf) -> anyhow::Result<()> {
    use dashboard::*;
    let cfg: Config = serde_json::from_str(&std::fs::read_to_string(config)?)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut families: Vec<String> = Vec::new();
    for p in &cfg.projects {
        if !families.contains(&p.family) {
            families.push(p.family.clone());
        }
    }
    let mut body = String::new();
    for fam in &families {
        body.push_str(&format!("<section><h2>{}</h2>", esc(fam)));
        for p in cfg.projects.iter().filter(|p| &p.family == fam) {
            let dir = base.join(&p.dir);
            let prov = provenance(&dir);
            let (n_results, newest) = newest_result(&dir);
            body.push_str("<article class=\"card\">");
            body.push_str(&format!(
                "<header><h3>{}</h3><div class=\"meta\">{}{} · {} result files, newest {}{}</div></header>",
                if p.repo_url.is_empty() { esc(&p.name) } else { format!("<a href=\"{}\">{}</a>", esc(&p.repo_url), esc(&p.name)) },
                if prov.commit.is_empty() { "no git".to_string() } else { format!("commit <code>{}</code> {}", esc(&prov.commit), esc(&prov.date)) },
                if prov.dirty { " <span class=\"warn\">uncommitted changes</span>" } else { "" },
                n_results,
                esc(&newest),
                if p.repo_url.is_empty() { String::new() } else { format!(" · <a href=\"{}/actions\">CI</a>", esc(&p.repo_url)) }
            ));
            if !p.note.is_empty() {
                body.push_str(&format!("<p class=\"note\">{}</p>", inline(&p.note)));
            }
            if p.sources.is_empty() {
                body.push_str("<p class=\"muted\">No results published yet.</p>");
            }
            for s in &p.sources {
                let html = match s {
                    Source::ReadmeBlock { start, end, file } => readme_block(&dir, file.as_deref(), start, end),
                    Source::Outcomes { glob, heading } => outcomes(&dir, glob, heading),
                    Source::Fields { glob, columns, heading } => fields(&dir, glob, columns, heading),
                };
                match html {
                    Ok(h) => body.push_str(&h),
                    Err(e) => body.push_str(&format!("<p class=\"missing\">source unavailable: {}</p>", esc(&e.to_string()))),
                }
            }
            body.push_str("</article>");
        }
        body.push_str("</section>");
    }
    let page = format!(
        r##"<title>{title}</title>
<style>
:root{{--bg:#f6f3ec;--fg:#141414;--muted:#6b6b6b;--rule:#d9d4ca;--card:#ffffff;--accent:#8a3f1b;--warn:#a06811;--bad:#a62a1f;color-scheme:light}}
@media (prefers-color-scheme: dark){{:root:not([data-theme="light"]){{--bg:#121212;--fg:#ededed;--muted:#a0a0a0;--rule:#2a2a2a;--card:#1a1a1a;--accent:#e38a5f;--warn:#e3a84a;--bad:#e6716b;color-scheme:dark}}}}
:root[data-theme="dark"]{{--bg:#121212;--fg:#ededed;--muted:#a0a0a0;--rule:#2a2a2a;--card:#1a1a1a;--accent:#e38a5f;--warn:#e3a84a;--bad:#e6716b;color-scheme:dark}}
*{{box-sizing:border-box}}body{{background:var(--bg);color:var(--fg);font-family:ui-sans-serif,system-ui,-apple-system,sans-serif;line-height:1.5;margin:0;padding-inline:16px;padding-block:1.5rem 3rem;font-size:15px}}
main{{max-width:64rem;margin:0 auto}}h1{{font-size:1.6rem;margin:0 0 .25rem;letter-spacing:-.01em}}.sub{{color:var(--muted);font-size:.9rem;margin:0 0 2rem}}
h2{{font-size:.75rem;letter-spacing:.1em;text-transform:uppercase;color:var(--muted);margin:2.25rem 0 .9rem;font-weight:600}}
.card{{background:var(--card);border:1px solid var(--rule);border-radius:6px;padding:.9rem 1rem;margin-bottom:.9rem;min-width:0}}
.card h3{{font-size:1.05rem;margin:0}}.card h4{{font-size:.8rem;letter-spacing:.06em;text-transform:uppercase;color:var(--muted);margin:.9rem 0 .4rem;font-weight:600}}
.meta{{font-size:.82rem;color:var(--muted);margin-top:.15rem}}.note{{font-size:.9rem;margin:.6rem 0 0;color:var(--fg)}}.muted{{color:var(--muted);font-size:.9rem}}
.warn{{color:var(--warn);font-weight:600}}.missing{{color:var(--bad);font-weight:600}}
.tbl{{overflow-x:auto;margin-top:.6rem}}table{{border-collapse:collapse;font-size:.86rem;font-variant-numeric:tabular-nums;min-width:100%}}
th,td{{padding:.35rem .6rem;text-align:left;border-bottom:1px solid var(--rule);vertical-align:top}}th{{font-size:.72rem;letter-spacing:.06em;text-transform:uppercase;color:var(--muted);font-weight:600}}
td.n,th.n{{text-align:right}}code{{font-family:ui-monospace,'SF Mono',Menlo,Consolas,monospace;font-size:.86em;color:var(--accent)}}a{{color:var(--accent);text-decoration:none}}a:hover{{text-decoration:underline}}
ul{{margin:.4rem 0;padding-left:1.2rem}}p{{margin:.4rem 0}}footer{{color:var(--muted);font-size:.8rem;font-style:italic;margin-top:2.5rem;padding-top:1rem;border-top:1px solid var(--rule)}}
</style>
<main>
<header><h1>{title}</h1><p class="sub">{subtitle} · rendered {now} by bench-report {ver}</p></header>
{body}
<footer>Every table above is read from a committed result file or from a README block that the repository's own renderer produced from committed results. Commit hashes are the state of each working tree at render time. Nothing here is typed by hand; re-render to refresh.</footer>
</main>"##,
        title = esc(&cfg.title),
        subtitle = esc(&cfg.subtitle),
        now = dashboard::fmt_utc(now as i64),
        ver = env!("CARGO_PKG_VERSION"),
        body = body,
    );
    std::fs::write(out, page)?;
    println!("wrote {} ({} projects, {} families)", out.display(), cfg.projects.len(), families.len());
    Ok(())
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
        Cmd::Dashboard { config, base, out } => render_dashboard(&config, &base, &out)?,
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
