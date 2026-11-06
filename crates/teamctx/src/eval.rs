//! `teamctx eval`: retrieval quality over the fictional demo (plan sections 9.1 and 14).
//!
//! The runner imports `fixtures/demo/dataset.json` into an in-memory store, runs every
//! case of `fixtures/demo/eval.toml` through three retrieval paths and scores them. The
//! dataset and the case file are embedded at build time, so a run needs no files and is
//! deterministic: the same build always prints the same report.
//!
//! # Retrieval paths
//!
//! Each path calls the code production uses; the eval adds no ranking of its own.
//!
//! - **`search`**: `core::query::parse(case.query)` (the question as typed), then
//!   [`crate::search::rank_candidates`], the function `teamctx search` runs: entries and
//!   messages are fetched, scored with `core::rank::score` (bm25, recency decay, entry
//!   boost) and sorted together. The merged list is split by kind, keeping its order, and
//!   each list is cut at [`CUTOFF`].
//! - **`context_for`**: as `search`, but the query is
//!   `core::query::context_query(case.query)` (lowercased words, stopwords and a leading
//!   git prefix dropped). Within each kind the order is the `core::rank` order, entries
//!   are reported separately from messages (entries first, plan section 9). Neighbours are
//!   not added: they are context for a reader, not retrieval hits.
//! - **`ask_retrieval`**: `teamctx_llm::Bundle::retrieve` with the default
//!   [`BundleLimits`], the step that feeds `/ask` (plan section 11): the all-terms query
//!   plus up to six single-term queries, merged and ranked with `core::rank`, bounded by
//!   item count and size, and expanded with thread neighbours. The bundle has no cutoff of
//!   its own (at most 24 items), so its whole content counts: recall is the share of gold
//!   ids anywhere in the bundle, and the reciprocal rank uses the prompt position inside
//!   the entry list (rank order) or the message list (conversation and time order, so it
//!   is a weak signal). Bot messages and excluded channels never enter a bundle.
//!
//! The clock is fixed ([`NOW`]) so recency decay, and with it the report, is reproducible.
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
//! six overall numbers of any tool is below the stored one (tolerance 0; the data is
//! deterministic, so a drop is a real change). `--write-baseline` rewrites the file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use teamctx_core::{
    citation::CitationId,
    id::{MessageId, Snowflake},
    query::{context_query, parse},
    rank,
    record::ShortId,
    repo::RepoError,
    time::UnixMillis,
};
use teamctx_llm::{Bundle, BundleLimits};
use teamctx_store::{
    Store,
    demo::{DemoError, DemoMap, import},
};

use crate::search::{Found, Ranked, Scorer, rank_candidates};

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
const SCHEMA_VERSION: u32 = 2;
/// The case file schema (unchanged by the baseline's `ask_retrieval` table).
const CASES_SCHEMA_VERSION: u32 = 1;
/// The fixed clock of every run: 2026-05-05T16:53:20Z, after the newest demo message, so
/// recency decay is the same on every machine and day.
const NOW: UnixMillis = UnixMillis::from_millis(1_778_000_000_000);
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

fn score<T: Ord>(ranked: &[T], gold: &BTreeSet<T>, k: usize) -> Option<Scores> {
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
    ask_retrieval: ToolReport,
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

/// The two ranked id lists a retrieval path returned for one case.
struct Retrieved {
    messages: Vec<MessageId>,
    entries: Vec<ShortId>,
}

impl Retrieved {
    /// Splits a merged ranking by kind, keeping its order.
    fn from_ranked(ranked: &[Ranked]) -> Self {
        let mut found = Self {
            messages: Vec::new(),
            entries: Vec::new(),
        };
        for item in ranked {
            match &item.found {
                Found::Entry(hit) => found.entries.push(hit.entry.short_id()),
                Found::Message(hit) => found.messages.push(hit.message.id),
            }
        }
        found
    }

    /// The bundle's sources, in prompt order.
    fn from_bundle(bundle: &Bundle) -> Self {
        let mut found = Self {
            messages: Vec::new(),
            entries: Vec::new(),
        };
        for source in bundle.sources() {
            match source.id() {
                CitationId::Message(id) => found.messages.push(id),
                CitationId::Entry(id) => found.entries.push(id),
            }
        }
        found
    }

    /// Scores each list against the gold of its collection, counting the first `k`.
    fn score(&self, gold: &Resolved, k: usize) -> CaseScore {
        CaseScore {
            messages: score(&self.messages, &gold.gold_messages, k),
            entries: score(&self.entries, &gold.gold_entries, k),
        }
    }
}

/// The three retrieval paths, all through production code (see the module docs).
#[derive(Debug, Clone, Copy)]
enum Tool {
    Search,
    ContextFor,
    AskRetrieval,
}

impl Tool {
    fn retrieve(
        self,
        store: &Store,
        case: &Case,
        scorer: Scorer,
    ) -> Result<(Retrieved, usize), EvalError> {
        let store_error = |error: RepoError| EvalError::Store(error.to_string());
        let ranked = |query, scorer| {
            rank_candidates(store, query, CUTOFF, NOW, scorer)
                .map(|ranked| Retrieved::from_ranked(&ranked))
                .map_err(store_error)
        };
        match self {
            Self::Search => {
                let typed = parse(&case.query).map_err(|_| EvalError::Query {
                    case: case.id.clone(),
                })?;
                Ok((ranked(&typed, scorer)?, CUTOFF as usize))
            }
            Self::ContextFor => Ok((
                ranked(&context_query(&case.query), scorer)?,
                CUTOFF as usize,
            )),
            Self::AskRetrieval => {
                let bundle = Bundle::retrieve(store, &case.query, NOW, &BundleLimits::default())
                    .map_err(store_error)?;
                // The bundle is bounded by its own limits, so everything in it counts.
                Ok((Retrieved::from_bundle(&bundle), usize::MAX))
            }
        }
    }
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
    if file.schema_version != CASES_SCHEMA_VERSION {
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

/// Imports the demo and measures the three tools over every case. `scorer` ranks `search`
/// and `context_for`; production passes `core::rank::score`, and the tests replace it
/// with a deliberately broken one to prove the baseline check notices. (`ask_retrieval`
/// ranks inside `Bundle::retrieve`, so it always uses `core::rank`.)
fn measure_with(scorer: Scorer) -> Result<Report, EvalError> {
    let file = load_cases()?;
    let (store, map) = import_demo()?;
    let (mut search, mut context_for, mut ask) = (Vec::new(), Vec::new(), Vec::new());
    for case in &file.cases {
        let gold = resolve(case, &map)?;
        let category = case.category.as_str();
        for (tool, scores) in [
            (Tool::Search, &mut search),
            (Tool::ContextFor, &mut context_for),
            (Tool::AskRetrieval, &mut ask),
        ] {
            let (found, k) = tool.retrieve(&store, case, scorer)?;
            scores.push((category, found.score(&gold, k)));
        }
    }
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        cases: file.cases.len(),
        search: group(&search),
        context_for: group(&context_for),
        ask_retrieval: group(&ask),
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
    print_tool(out, "context_for", &report.context_for)?;
    print_tool(out, "ask_retrieval", &report.ask_retrieval)
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
        (
            "ask_retrieval",
            &baseline.ask_retrieval,
            &current.ask_retrieval,
        ),
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
# Every tool runs production retrieval (core::rank, Bundle::retrieve); ask_retrieval counts
# the whole bundle, the others the first 10 of each collection.
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
    execute_with(options, out, rank::score)
}

fn execute_with(
    options: &Options,
    out: &mut impl Write,
    scorer: Scorer,
) -> Result<bool, EvalError> {
    let report = measure_with(scorer)?;
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
        let ranked = rank_candidates(&store, &query, CUTOFF, NOW, rank::score)
            .map_err(|e| EvalError::Store(e.to_string()))?;
        let scored = Retrieved::from_ranked(&ranked).score(&gold, CUTOFF as usize);
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
            context_for: tool.clone(),
            ask_retrieval: tool,
        }
    }

    #[test]
    fn regressions_flag_only_drops_with_zero_tolerance() {
        let base = report_with(0.5);
        assert!(regressions(&base, &report_with(0.5)).is_empty());
        assert!(regressions(&base, &report_with(0.6)).is_empty());
        let found = regressions(&base, &report_with(0.4999));
        // All three tools carry the dropped number.
        assert_eq!(found.len(), 3, "{found:?}");
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

#[cfg(test)]
mod production_path_tests {
    use super::*;

    /// A ranking that puts the worst match first: every score negated.
    fn reversed(bm25: f64, created_at: UnixMillis, now: UnixMillis, is_record: bool) -> f64 {
        -rank::score(bm25, created_at, now, is_record)
    }

    fn options(mode: Mode, baseline: &Path) -> Options {
        Options {
            mode,
            baseline: baseline.to_path_buf(),
        }
    }

    #[test]
    fn a_reversed_ranking_fails_the_baseline_check() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("baseline.toml");
        let mut sink = Vec::new();
        assert!(execute_with(
            &options(Mode::WriteBaseline, &path),
            &mut sink,
            rank::score
        )?);

        // The same ranking passes the check, a reversed one does not.
        let check = options(Mode::Check, &path);
        let mut good = Vec::new();
        assert!(execute_with(&check, &mut good, rank::score)?);
        let mut bad = Vec::new();
        assert!(!execute_with(&check, &mut bad, reversed)?);
        let report = String::from_utf8(bad)?;
        assert!(report.contains("FAIL"), "{report}");
        assert!(report.contains("\n  search "), "names search: {report}");
        assert!(
            report.contains("\n  context_for "),
            "names context_for: {report}"
        );
        Ok(())
    }

    #[test]
    fn ask_retrieval_returns_the_bundle_with_neighbours() -> Result<(), EvalError> {
        let file = load_cases()?;
        let (store, map) = import_demo()?;
        let case = file
            .cases
            .iter()
            .find(|case| !case.expected_messages.is_empty())
            .ok_or(EvalError::Cases("no case with message gold"))?;
        let gold = resolve(case, &map)?;
        let (found, k) = Tool::AskRetrieval.retrieve(&store, case, rank::score)?;
        assert_eq!(k, usize::MAX);
        assert!(found.messages.len() + found.entries.len() <= BundleLimits::default().max_items);
        // A bundle adds thread neighbours, so it holds messages the ranked search does not.
        let search = Tool::ContextFor.retrieve(&store, case, rank::score)?.0;
        let extra = found
            .messages
            .iter()
            .filter(|id| !search.messages.contains(id))
            .count();
        assert!(extra > 0, "no neighbour or extra message in the bundle");
        assert!(found.score(&gold, k).messages.is_some());
        Ok(())
    }

    #[test]
    fn the_report_has_a_row_for_every_tool() -> Result<(), EvalError> {
        let report = measure_with(rank::score)?;
        for tool in [&report.search, &report.context_for, &report.ask_retrieval] {
            assert_eq!(tool.get(OVERALL).map(|m| m.cases), Some(report.cases));
        }
        let mut out = Vec::new();
        print_report(&mut out, &report)?;
        assert!(String::from_utf8_lossy(&out).contains("ask_retrieval"));
        Ok(())
    }
}
