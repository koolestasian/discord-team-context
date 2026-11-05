//! `teamctx eval`: retrieval quality over the fictional demo (plan sections 9.1 and 14).
//!
//! The runner imports `fixtures/demo/dataset.json` into an in-memory store, runs every
//! case of `fixtures/demo/eval.toml` through two retrieval paths and scores them. The
//! dataset and the case file are embedded at build time, so a run needs no files and is
//! deterministic: the same build always prints the same report.
//!
//! # Retrieval paths
//!
//! - **`search`**: `core::query::parse(case.query)`, then `search_entries` and
//!   `search_messages`. The query is the natural-language question as typed, so it keeps
//!   its stopwords and every word must match (clauses combine with AND).
//! - **`context_for`**: `core::query::context_query(case.query)` (lowercased words,
//!   stopwords and a leading git prefix dropped), then `search_entries`, then
//!   `search_messages` (entries first, plan section 9). Neighbouring messages are not
//!   added: they are context for a reader, not retrieval hits.
//!
//! Each collection returns its own list in the store's order (best `bm25` first), cut at
//! [`CUTOFF`] items. The recency and entry-boost ranking of `core::rank` is not applied.
//!
//! # Metric definitions
//!
//! Gold ids come from the case: `expected_messages` (snowflakes) are mapped through
//! [`DemoMap::message`] and `expected_entries` (fixture short ids such as `D-7`) through
//! [`DemoMap::entry_label`]. Messages are scored only against the message gold and
//! entries only against the entry gold, so a collection is never penalised for ids it
//! cannot return. A collection with no gold in a case is skipped for that case.
//!
//! For one case and one collection, with `top` the first 10 results and `gold` the set
//! of expected ids:
//!
//! - **recall@10** = `|top ∩ gold| / |gold|`.
//! - **reciprocal rank** = `1 / r`, where `r` is the 1-based position in `top` of the
//!   first gold id, or `0` when no gold id is in `top`.
//!
//! **MRR** is the mean reciprocal rank over cases. A case's *combined* value is the mean
//! of its message and entry values (only the collections that have gold); the combined
//! metrics are the mean of those over cases. The report gives, per tool and per category
//! and over all cases, six numbers: message, entry and combined recall@10 and MRR. Means
//! are over the cases of the group and rounded to four decimals.
//!
//! # Baseline
//!
//! `fixtures/demo/eval-baseline.toml` stores the report. `--check` fails when any of the
//! six overall numbers of either tool is below the stored one (tolerance 0; the data is
//! deterministic, so a drop is a real change). `--write-baseline` rewrites the file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use teamctx_core::{
    id::{MessageId, Snowflake},
    query::{Query, context_query, parse},
    record::ShortId,
    repo::{MessageRead, RecordRead},
};
use teamctx_store::{
    Store,
    demo::{DemoError, DemoMap, import},
};

/// The dataset, embedded at build time.
const DATASET: &str = include_str!("../../../fixtures/demo/dataset.json");
/// The evaluation cases, embedded at build time.
const CASES: &str = include_str!("../../../fixtures/demo/eval.toml");
/// Where the baseline lives in the repository (developer subcommand, run from a checkout).
const DEFAULT_BASELINE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/demo/eval-baseline.toml"
);
/// The result cutoff of recall@10 and MRR.
const CUTOFF: u32 = 10;
/// The case file and baseline schema this runner reads and writes.
const SCHEMA_VERSION: u32 = 1;
/// Key of the all-cases row in a tool's table.
const OVERALL: &str = "overall";

/// What `teamctx eval` should do after measuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Print the report only.
    Report,
    /// Print the report and fail if an overall number is below the baseline.
    Check,
    /// Print the report and rewrite the baseline file.
    WriteBaseline,
}

/// Parsed `teamctx eval` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    mode: Mode,
    baseline: PathBuf,
}

/// An argument the `eval` command does not accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArgError {
    Unknown(OsString),
    MissingValue(&'static str),
    Conflict,
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `{:?}` escapes control characters in what the user typed.
            Self::Unknown(arg) => write!(f, "unrecognised eval argument {arg:?}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::Conflict => f.write_str("--check and --write-baseline cannot be combined"),
        }
    }
}

/// Parses the arguments after `eval`: `--check`, `--write-baseline`, `--baseline PATH`.
pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Options, ArgError> {
    let (mut check, mut write) = (false, false);
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--check") => check = true,
            Some("--write-baseline") => write = true,
            Some("--baseline") => {
                baseline = args
                    .next()
                    .ok_or(ArgError::MissingValue("--baseline"))?
                    .into();
            }
            _ => return Err(ArgError::Unknown(arg)),
        }
    }
    let mode = match (check, write) {
        (true, true) => return Err(ArgError::Conflict),
        (true, false) => Mode::Check,
        (false, true) => Mode::WriteBaseline,
        (false, false) => Mode::Report,
    };
    Ok(Options { mode, baseline })
}

/// Why the evaluation could not run (not: why quality is too low).
#[derive(Debug)]
pub(crate) enum EvalError {
    /// The embedded case file is unusable.
    Cases(&'static str),
    /// The demo dataset could not be imported.
    Import(DemoError),
    /// A case names an id the dataset does not have.
    UnknownGold { case: String },
    /// A case query is not valid query syntax.
    Query { case: String },
    /// The store failed while searching.
    Store(String),
    /// The baseline file could not be read, parsed or written.
    Baseline(String),
    /// Writing the report failed.
    Output(io::Error),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cases(reason) => write!(f, "eval cases: {reason}"),
            Self::Import(error) => write!(f, "demo import failed: {error}"),
            Self::UnknownGold { case } => write!(f, "case {case}: expected id not in the dataset"),
            Self::Query { case } => write!(f, "case {case}: query is not valid query syntax"),
            Self::Store(message) => write!(f, "search failed: {message}"),
            Self::Baseline(message) => write!(f, "baseline: {message}"),
            Self::Output(error) => write!(f, "cannot write the report: {error}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<io::Error> for EvalError {
    fn from(error: io::Error) -> Self {
        Self::Output(error)
    }
}

#[derive(Debug, Deserialize)]
struct CaseFile {
    schema_version: u32,
    cutoff: u32,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    category: String,
    query: String,
    expected_messages: Vec<String>,
    expected_entries: Vec<String>,
}

/// Recall@`k` of one ranked list: the share of distinct gold ids found in the first `k`.
/// `None` when there is no gold, so the collection is skipped rather than scored 0.
fn recall_at<T: Ord>(ranked: &[T], gold: &BTreeSet<T>, k: usize) -> Option<f64> {
    if gold.is_empty() {
        return None;
    }
    let found: BTreeSet<&T> = ranked
        .iter()
        .take(k)
        .filter(|id| gold.contains(id))
        .collect();
    Some(found.len() as f64 / gold.len() as f64)
}

/// Reciprocal rank of one ranked list: `1 / position` of the first gold id within the
/// first `k`, else 0. `None` when there is no gold.
fn reciprocal_rank<T: Ord>(ranked: &[T], gold: &BTreeSet<T>, k: usize) -> Option<f64> {
    if gold.is_empty() {
        return None;
    }
    Some(
        ranked
            .iter()
            .take(k)
            .position(|id| gold.contains(id))
            .map_or(0.0, |index| 1.0 / (index + 1) as f64),
    )
}

/// Mean of the values that exist; `None` when none do.
fn mean(values: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    let (sum, count) = values
        .into_iter()
        .flatten()
        .fold((0.0, 0_usize), |(sum, count), v| (sum + v, count + 1));
    (count > 0).then(|| sum / count as f64)
}

/// Rounds to four decimals, the precision of the report and the baseline.
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// One collection's scores for one case.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scores {
    recall: f64,
    rr: f64,
}

fn score<T: Ord>(ranked: &[T], gold: &BTreeSet<T>) -> Option<Scores> {
    let k = CUTOFF as usize;
    Some(Scores {
        recall: recall_at(ranked, gold, k)?,
        rr: reciprocal_rank(ranked, gold, k)?,
    })
}

/// Both collections' scores for one case under one tool.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CaseScore {
    messages: Option<Scores>,
    entries: Option<Scores>,
}

impl CaseScore {
    /// Mean of the collections that have gold in this case.
    fn combined(&self, field: fn(&Scores) -> f64) -> Option<f64> {
        mean([
            self.messages.as_ref().map(field),
            self.entries.as_ref().map(field),
        ])
    }
}

fn recall(scores: &Scores) -> f64 {
    scores.recall
}

fn rr(scores: &Scores) -> f64 {
    scores.rr
}

/// Six numbers over a group of cases: message, entry and combined recall@10 and MRR.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Metrics {
    cases: usize,
    messages_recall_at_10: f64,
    messages_mrr: f64,
    entries_recall_at_10: f64,
    entries_mrr: f64,
    combined_recall_at_10: f64,
    combined_mrr: f64,
}

impl Metrics {
    fn of(group: &[&CaseScore]) -> Self {
        let average = |value: &dyn Fn(&CaseScore) -> Option<f64>| {
            round4(mean(group.iter().map(|case| value(case))).unwrap_or(0.0))
        };
        Self {
            cases: group.len(),
            messages_recall_at_10: average(&|c| c.messages.as_ref().map(recall)),
            messages_mrr: average(&|c| c.messages.as_ref().map(rr)),
            entries_recall_at_10: average(&|c| c.entries.as_ref().map(recall)),
            entries_mrr: average(&|c| c.entries.as_ref().map(rr)),
            combined_recall_at_10: average(&|c| c.combined(recall)),
            combined_mrr: average(&|c| c.combined(rr)),
        }
    }

    /// The six scores by name, in report order.
    fn values(&self) -> [(&'static str, f64); 6] {
        [
            ("messages_recall_at_10", self.messages_recall_at_10),
            ("messages_mrr", self.messages_mrr),
            ("entries_recall_at_10", self.entries_recall_at_10),
            ("entries_mrr", self.entries_mrr),
            ("combined_recall_at_10", self.combined_recall_at_10),
            ("combined_mrr", self.combined_mrr),
        ]
    }
}

/// Per-group metrics of one tool: the key `overall` plus one entry per category.
type ToolReport = BTreeMap<String, Metrics>;

/// The stored form of a report, also the file `fixtures/demo/eval-baseline.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Report {
    schema_version: u32,
    cases: usize,
    search: ToolReport,
    context_for: ToolReport,
}

fn group(per_case: &[(&str, CaseScore)]) -> ToolReport {
    let mut categories: BTreeMap<&str, Vec<&CaseScore>> = BTreeMap::new();
    for (category, score) in per_case {
        categories.entry(category).or_default().push(score);
    }
    let mut report: ToolReport = categories
        .into_iter()
        .map(|(category, scores)| (category.to_owned(), Metrics::of(&scores)))
        .collect();
    let all: Vec<&CaseScore> = per_case.iter().map(|(_, score)| score).collect();
    report.insert(OVERALL.to_owned(), Metrics::of(&all));
    report
}

/// Runs one text-query retrieval path and scores it against the case's gold ids.
fn run_tool(
    store: &Store,
    query: &Query,
    gold_messages: &BTreeSet<MessageId>,
    gold_entries: &BTreeSet<ShortId>,
) -> Result<CaseScore, EvalError> {
    let store_error = |error: teamctx_core::repo::RepoError| EvalError::Store(error.to_string());
    // Entries are searched first, as `context_for` does (plan section 9).
    let entries: Vec<ShortId> = store
        .search_entries(query, CUTOFF)
        .map_err(store_error)?
        .into_iter()
        .map(|hit| ShortId {
            kind: hit.entry.kind,
            id: hit.entry.id,
        })
        .collect();
    let messages: Vec<MessageId> = store
        .search_messages(query, CUTOFF)
        .map_err(store_error)?
        .into_iter()
        .map(|hit| hit.message.id)
        .collect();
    Ok(CaseScore {
        messages: score(&messages, gold_messages),
        entries: score(&entries, gold_entries),
    })
}

/// A case with its gold ids mapped to the stored rows.
struct Resolved {
    gold_messages: BTreeSet<MessageId>,
    gold_entries: BTreeSet<ShortId>,
}

fn resolve(case: &Case, map: &DemoMap) -> Result<Resolved, EvalError> {
    let unknown = || EvalError::UnknownGold {
        case: case.id.clone(),
    };
    let gold_messages = case
        .expected_messages
        .iter()
        .map(|id| {
            let external = id.parse::<Snowflake>().map_err(|_| unknown())?;
            map.message(external).ok_or_else(unknown)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let gold_entries = case
        .expected_entries
        .iter()
        .map(|label| map.entry_label(label).ok_or_else(unknown))
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(Resolved {
        gold_messages,
        gold_entries,
    })
}

fn load_cases() -> Result<CaseFile, EvalError> {
    let file: CaseFile = toml::from_str(CASES)
        .map_err(|_| EvalError::Cases("not valid TOML of the expected shape"))?;
    if file.schema_version != SCHEMA_VERSION {
        return Err(EvalError::Cases("unsupported schema version"));
    }
    if file.cutoff != CUTOFF {
        return Err(EvalError::Cases("cutoff is not 10"));
    }
    Ok(file)
}

fn import_demo() -> Result<(Store, DemoMap), EvalError> {
    let mut store = Store::in_memory().map_err(|e| EvalError::Store(e.to_string()))?;
    let map = import(&mut store, DATASET).map_err(EvalError::Import)?;
    Ok((store, map))
}

/// Imports the demo and measures both tools over every case.
pub(crate) fn measure() -> Result<Report, EvalError> {
    let file = load_cases()?;
    let (store, map) = import_demo()?;
    let (mut search, mut context_for) = (Vec::new(), Vec::new());
    for case in &file.cases {
        let gold = resolve(case, &map)?;
        let typed = parse(&case.query).map_err(|_| EvalError::Query {
            case: case.id.clone(),
        })?;
        let keywords = context_query(&case.query);
        let category = case.category.as_str();
        let (m, e) = (&gold.gold_messages, &gold.gold_entries);
        search.push((category, run_tool(&store, &typed, m, e)?));
        context_for.push((category, run_tool(&store, &keywords, m, e)?));
    }
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        cases: file.cases.len(),
        search: group(&search),
        context_for: group(&context_for),
    })
}

fn print_tool(out: &mut impl Write, name: &str, tool: &ToolReport) -> io::Result<()> {
    writeln!(out, "\n{name}")?;
    writeln!(
        out,
        "{:<14} {:>5}  {:>8} {:>8}  {:>8} {:>8}  {:>8} {:>8}",
        "group", "cases", "msg R@10", "msg MRR", "ent R@10", "ent MRR", "all R@10", "all MRR"
    )?;
    let overall = tool.get_key_value(OVERALL);
    let rest = tool.iter().filter(|(key, _)| key.as_str() != OVERALL);
    for (key, m) in overall.into_iter().chain(rest) {
        writeln!(
            out,
            "{:<14} {:>5}  {:>8.4} {:>8.4}  {:>8.4} {:>8.4}  {:>8.4} {:>8.4}",
            key,
            m.cases,
            m.messages_recall_at_10,
            m.messages_mrr,
            m.entries_recall_at_10,
            m.entries_mrr,
            m.combined_recall_at_10,
            m.combined_mrr
        )?;
    }
    Ok(())
}

/// Prints the report as fixed-width tables.
pub(crate) fn print_report(out: &mut impl Write, report: &Report) -> io::Result<()> {
    writeln!(
        out,
        "Retrieval eval over the demo dataset: {} cases, cutoff {CUTOFF}",
        report.cases
    )?;
    print_tool(out, "search", &report.search)?;
    print_tool(out, "context_for", &report.context_for)
}

/// Integer ten-thousandths, so equal rounded values compare equal exactly.
fn units(value: f64) -> i64 {
    (value * 10_000.0).round() as i64
}

/// The overall numbers of `current` that are below `baseline`, as readable lines.
/// An empty result means no regression. Rises are not reported.
pub(crate) fn regressions(baseline: &Report, current: &Report) -> Vec<String> {
    let mut found = Vec::new();
    let tools = [
        ("search", &baseline.search, &current.search),
        ("context_for", &baseline.context_for, &current.context_for),
    ];
    for (tool, base, now) in tools {
        let (Some(base), Some(now)) = (base.get(OVERALL), now.get(OVERALL)) else {
            found.push(format!("{tool}: no overall row to compare"));
            continue;
        };
        for ((name, was), (_, is)) in base.values().into_iter().zip(now.values()) {
            if units(is) < units(was) {
                found.push(format!(
                    "{tool} {name}: {is:.4} is below the baseline {was:.4}"
                ));
            }
        }
    }
    found
}

const BASELINE_HEADER: &str = "\
# Retrieval baseline for the fictional demo, written by `cargo xtask eval -- --write-baseline`.
# CI fails when an overall number falls below this file. Definitions: crates/teamctx/src/eval.rs.
# Messages are scored against expected_messages and entries against expected_entries.
";

fn read_baseline(path: &Path) -> Result<Report, EvalError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| EvalError::Baseline(format!("cannot read {}: {e}", path.display())))?;
    let report: Report = toml::from_str(&text)
        .map_err(|_| EvalError::Baseline(format!("{} is not a valid baseline", path.display())))?;
    if report.schema_version != SCHEMA_VERSION {
        return Err(EvalError::Baseline("unsupported schema version".to_owned()));
    }
    Ok(report)
}

fn write_baseline(path: &Path, report: &Report) -> Result<(), EvalError> {
    let body = toml::to_string(report)
        .map_err(|_| EvalError::Baseline("cannot serialise the report".to_owned()))?;
    std::fs::write(path, format!("{BASELINE_HEADER}{body}"))
        .map_err(|e| EvalError::Baseline(format!("cannot write {}: {e}", path.display())))
}

/// Runs `teamctx eval`. `Ok(false)` means a regression against the baseline (exit 1).
pub(crate) fn execute(options: &Options, out: &mut impl Write) -> Result<bool, EvalError> {
    let report = measure()?;
    print_report(out, &report)?;
    match options.mode {
        Mode::Report => Ok(true),
        Mode::WriteBaseline => {
            write_baseline(&options.baseline, &report)?;
            writeln!(out, "\nbaseline written to {}", options.baseline.display())?;
            Ok(true)
        }
        Mode::Check => {
            let baseline = read_baseline(&options.baseline)?;
            if baseline.cases != report.cases {
                return Err(EvalError::Baseline(format!(
                    "the baseline covers {} cases but the eval set has {}; rerun with --write-baseline",
                    baseline.cases, report.cases
                )));
            }
            let found = regressions(&baseline, &report);
            if found.is_empty() {
                writeln!(out, "\nOK: no overall metric is below the baseline")?;
                return Ok(true);
            }
            writeln!(out, "\nFAIL: retrieval quality fell below the baseline")?;
            for line in &found {
                writeln!(out, "  {line}")?;
            }
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipeline_finds_gold_when_the_query_matches_the_evidence() -> Result<(), EvalError> {
        // Guards against an all-zero report caused by a wiring bug rather than by weak
        // retrieval: a query copied from the evidence must hit the gold of para-01.
        let file = load_cases()?;
        let (store, map) = import_demo()?;
        let case = file
            .cases
            .iter()
            .find(|case| case.id == "para-01")
            .ok_or(EvalError::Cases("para-01 missing"))?;
        let gold = resolve(case, &map)?;
        let query = parse("Fenwick Serif").map_err(|_| EvalError::Query {
            case: case.id.clone(),
        })?;
        let scored = run_tool(&store, &query, &gold.gold_messages, &gold.gold_entries)?;
        let messages = scored.messages.ok_or(EvalError::Cases("no message gold"))?;
        assert!(messages.recall > 0.0 && messages.rr > 0.0, "{messages:?}");
        Ok(())
    }

    fn set(ids: &[u32]) -> BTreeSet<u32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn recall_counts_distinct_gold_in_the_first_k() {
        let gold = set(&[1, 2, 3, 4]);
        // Top 4 is [9, 2, 2, 1]: gold 2 and 1 found (2 counted once) of four.
        assert_eq!(recall_at(&[9, 2, 2, 1, 3], &gold, 4), Some(0.5));
        assert_eq!(recall_at(&[9, 8, 7], &gold, 3), Some(0.0));
        assert_eq!(recall_at(&[1, 2, 3, 4], &gold, 10), Some(1.0));
        // Gold beyond the cutoff is not found.
        assert_eq!(recall_at(&[9, 8, 1], &gold, 2), Some(0.0));
        // No gold: the collection is skipped, not scored zero.
        assert_eq!(recall_at(&[1, 2], &BTreeSet::new(), 10), None);
        assert_eq!(recall_at::<u32>(&[], &gold, 10), Some(0.0));
    }

    #[test]
    fn reciprocal_rank_uses_the_first_gold_position_within_k() {
        let gold = set(&[5, 6]);
        assert_eq!(reciprocal_rank(&[5, 1, 2], &gold, 10), Some(1.0));
        assert_eq!(reciprocal_rank(&[1, 6, 5], &gold, 10), Some(0.5));
        assert_eq!(reciprocal_rank(&[1, 2, 3, 6], &gold, 10), Some(0.25));
        assert_eq!(reciprocal_rank(&[1, 2, 3, 6], &gold, 3), Some(0.0));
        assert_eq!(reciprocal_rank(&[1, 2], &gold, 10), Some(0.0));
        assert_eq!(reciprocal_rank(&[1, 2], &BTreeSet::new(), 10), None);
    }

    #[test]
    fn metrics_average_cases_and_skip_missing_collections() {
        let full = |recall, rr| Some(Scores { recall, rr });
        let a = CaseScore {
            messages: full(1.0, 1.0),
            entries: full(0.0, 0.0),
        };
        let b = CaseScore {
            messages: full(0.5, 0.5),
            entries: None,
        };
        let m = Metrics::of(&[&a, &b]);
        assert_eq!(m.cases, 2);
        assert_eq!(m.messages_recall_at_10, 0.75);
        assert_eq!(m.messages_mrr, 0.75);
        // Only case a has entry gold.
        assert_eq!(m.entries_recall_at_10, 0.0);
        // Combined: case a = (1 + 0) / 2 = 0.5, case b = 0.5 (messages only).
        assert_eq!(m.combined_recall_at_10, 0.5);
        assert_eq!(m.combined_mrr, 0.5);
        assert_eq!(round4(2.0 / 3.0), 0.6667);
    }

    fn report_with(messages_mrr: f64) -> Report {
        let metrics = Metrics {
            cases: 1,
            messages_recall_at_10: 0.5,
            messages_mrr,
            entries_recall_at_10: 0.5,
            entries_mrr: 0.5,
            combined_recall_at_10: 0.5,
            combined_mrr: 0.5,
        };
        let tool: ToolReport = [(OVERALL.to_owned(), metrics)].into_iter().collect();
        Report {
            schema_version: SCHEMA_VERSION,
            cases: 1,
            search: tool.clone(),
            context_for: tool,
        }
    }

    #[test]
    fn regressions_flag_only_drops_with_zero_tolerance() {
        let base = report_with(0.5);
        assert!(regressions(&base, &report_with(0.5)).is_empty());
        assert!(regressions(&base, &report_with(0.6)).is_empty());
        let found = regressions(&base, &report_with(0.4999));
        // Both tools carry the dropped number.
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("search messages_mrr"), "{found:?}");
    }

    #[test]
    fn arguments_select_a_mode_and_reject_the_rest() {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        assert_eq!(args(&[]).map(|o| o.mode), Ok(Mode::Report));
        assert_eq!(args(&["--check"]).map(|o| o.mode), Ok(Mode::Check));
        assert_eq!(
            args(&["--write-baseline"]).map(|o| o.mode),
            Ok(Mode::WriteBaseline)
        );
        assert_eq!(
            args(&["--check", "--write-baseline"]),
            Err(ArgError::Conflict)
        );
        assert_eq!(
            args(&["--baseline"]),
            Err(ArgError::MissingValue("--baseline"))
        );
        assert_eq!(
            args(&["--bogus"]),
            Err(ArgError::Unknown(OsString::from("--bogus")))
        );
        assert_eq!(
            args(&["--baseline", "x.toml"]).map(|o| o.baseline),
            Ok(PathBuf::from("x.toml"))
        );
    }
}
