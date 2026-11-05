//! End-to-end checks of `teamctx eval`: the whole run is deterministic, the committed
//! baseline passes, and a baseline above the measured numbers fails the check.

use std::path::Path;
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BASELINE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/demo/eval-baseline.toml"
);

fn teamctx(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .arg("eval")
        .args(args)
        .output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn two_runs_print_identical_reports() -> TestResult {
    let first = teamctx(&[])?;
    let second = teamctx(&[])?;
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert_eq!(first.stdout, second.stdout);
    let report = text(&first.stdout);
    for expected in ["62 cases", "search", "context_for", "overall", "paraphrase"] {
        assert!(report.contains(expected), "missing {expected:?}: {report}");
    }
    assert!(first.stderr.is_empty());
    Ok(())
}

#[test]
fn the_committed_baseline_passes_the_check() -> TestResult {
    let output = teamctx(&["--check"])?;
    assert!(
        output.status.success(),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(text(&output.stdout).contains("OK: no overall metric is below the baseline"));
    Ok(())
}

/// The committed baseline with every metric raised above anything attainable.
fn impossible_baseline(committed: &str) -> String {
    committed
        .lines()
        .map(|line| match line.split_once(" = ") {
            Some((key, _)) if key.ends_with("_at_10") || key.ends_with("_mrr") => {
                format!("{key} = 2.0")
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_baseline_above_the_measured_numbers_fails_the_check() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("baseline.toml");
    std::fs::write(
        &path,
        impossible_baseline(&std::fs::read_to_string(BASELINE)?),
    )?;
    let output = teamctx(&["--check", "--baseline", path_str(&path)?])?;
    assert_eq!(output.status.code(), Some(1));
    let report = text(&output.stdout);
    assert!(report.contains("FAIL"), "{report}");
    assert!(
        report.contains("search messages_mrr: "),
        "names the metric: {report}"
    );
    Ok(())
}

#[test]
fn write_baseline_produces_a_file_the_check_accepts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("baseline.toml");
    let path = path_str(&path)?;
    let written = teamctx(&["--write-baseline", "--baseline", path])?;
    assert!(written.status.success(), "{}", text(&written.stderr));
    let checked = teamctx(&["--check", "--baseline", path])?;
    assert!(checked.status.success(), "{}", text(&checked.stdout));
    Ok(())
}

#[test]
fn bad_arguments_and_a_missing_baseline_are_reported() -> TestResult {
    let conflict = teamctx(&["--check", "--write-baseline"])?;
    assert_eq!(conflict.status.code(), Some(2));
    assert!(conflict.stdout.is_empty());
    let unknown = teamctx(&["--bogus"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(text(&unknown.stderr).starts_with("error: "));

    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("absent.toml");
    let output = teamctx(&["--check", "--baseline", path_str(&missing)?])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("baseline"));
    Ok(())
}

fn path_str(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    path.to_str().ok_or_else(|| "non-UTF-8 temp path".into())
}
