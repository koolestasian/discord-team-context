//! End-to-end checks of `teamctx export` on the fictional demo record.
//!
//! The expected bundles live as plain files under `tests/snapshots/`. To refresh them
//! after an intended change, run the tests with `TEAMCTX_UPDATE_SNAPSHOTS=1` and review
//! the diff. All data is the fictional demo dataset.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn teamctx(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .args(args)
        .output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_arg(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    path.to_str().ok_or_else(|| "non-UTF-8 test path".into())
}

/// A demo database in a fresh temporary directory.
fn demo_db(dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let db = dir.join("demo.db");
    let output = teamctx(&["demo", "--db", path_arg(&db)?])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    Ok(db)
}

/// Every file under `root` as `relative/path -> content`, with `\n` line endings.
fn read_tree(root: &Path) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(&dir)? {
            let path = item?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)?
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                files.insert(relative, fs::read_to_string(&path)?.replace("\r\n", "\n"));
            }
        }
    }
    Ok(files)
}

fn snapshot_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(name)
}

/// Compares `actual` with the snapshot directory, or rewrites it when asked to.
fn check_snapshot(name: &str, actual: &Path) -> TestResult {
    let expected_dir = snapshot_dir(name);
    let produced = read_tree(actual)?;
    if std::env::var_os("TEAMCTX_UPDATE_SNAPSHOTS").is_some() {
        if expected_dir.exists() {
            fs::remove_dir_all(&expected_dir)?;
        }
        for (relative, content) in &produced {
            let target = expected_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, content)?;
        }
        return Ok(());
    }
    let expected = read_tree(&expected_dir)?;
    let produced_names: Vec<_> = produced.keys().collect();
    let expected_names: Vec<_> = expected.keys().collect();
    assert_eq!(produced_names, expected_names, "{name}: file set differs");
    for (relative, content) in &produced {
        assert_eq!(
            content, &expected[relative],
            "{name}/{relative} differs; rerun with TEAMCTX_UPDATE_SNAPSHOTS=1 if intended"
        );
    }
    Ok(())
}

#[test]
fn the_okf_bundle_of_the_demo_matches_its_snapshot() -> TestResult {
    let work = tempfile::tempdir()?;
    let db = demo_db(work.path())?;
    let out = work.path().join("record");
    let output = teamctx(&[
        "export",
        "--format",
        "okf",
        path_arg(&out)?,
        "--db",
        path_arg(&db)?,
    ])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("76 entries, 78 files, OKF 0.2 bundle"));
    check_snapshot("export-okf", &out)
}

#[test]
fn the_markdown_export_of_the_demo_matches_its_snapshot() -> TestResult {
    let work = tempfile::tempdir()?;
    let db = demo_db(work.path())?;
    let out = work.path().join("record");
    let output = teamctx(&[
        "export",
        "--format",
        "md",
        path_arg(&out)?,
        "--db",
        path_arg(&db)?,
    ])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("76 entries, 1 files, Markdown file"));
    check_snapshot("export-md", &out)
}

#[test]
fn the_demo_bundle_is_self_consistent() -> TestResult {
    let work = tempfile::tempdir()?;
    let db = demo_db(work.path())?;
    let out = work.path().join("record");
    let output = teamctx(&[
        "export",
        "--format",
        "okf",
        path_arg(&out)?,
        "--db",
        path_arg(&db)?,
    ])?;
    assert!(output.status.success());
    let tree = read_tree(&out)?;
    let entries: Vec<(&String, &String)> =
        tree.iter().filter(|(path, _)| path.contains('/')).collect();
    assert_eq!(entries.len(), 76);
    let index = &tree["index.md"];
    assert!(index.starts_with("---\nokf_version: \"0.2\"\n---\n"));
    let mut superseded = 0;
    for (path, content) in entries {
        // Every file has frontmatter with a type, and the index lists it.
        assert!(content.starts_with("---\ntype: "), "{path}");
        assert!(
            index.contains(&format!("(/{path})")),
            "{path} missing from the index"
        );
        let (front, body) = content[4..].split_once("\n---\n\n").ok_or(path.as_str())?;
        // Footnote labels equal sources[].id.
        for label in body.match_indices("[^").map(|(at, _)| {
            let rest = &body[at + 2..];
            rest[..rest.find(']').unwrap_or(0)].to_owned()
        }) {
            assert!(
                front.contains(&format!("  - id: {label}\n")),
                "{path}: [^{label}]"
            );
        }
        // A superseded decision links forward, and the decision it names links back.
        if let Some(target) = front
            .lines()
            .find_map(|line| line.strip_prefix("superseded_by: \""))
            .and_then(|rest| rest.strip_suffix('"'))
        {
            superseded += 1;
            let newer = &tree[target.trim_start_matches('/')];
            assert!(
                newer.contains(&format!("supersedes:\n  - \"/{path}\"\n"))
                    || newer.contains(&format!("\n  - \"/{path}\"\n")),
                "{target} does not link back to {path}"
            );
        }
        assert!(
            front.contains("\nverified: { by: \"human:discord-"),
            "{path}"
        );
        assert!(
            front.contains("\ngenerated: { by: \"human:discord-"),
            "{path}"
        );
    }
    assert_eq!(superseded, 14, "the demo reverses 14 decisions");
    let log = &tree["log.md"];
    assert!(log.starts_with("# Record change log\n"));
    assert!(log.contains("\n## 2026-04-22\n"));
    // The demo's planted injection text is chat, and chat is not copied into the bundle.
    for content in tree.values() {
        assert!(!content.contains("Ignore all previous instructions"));
    }
    Ok(())
}

#[test]
fn a_non_empty_directory_needs_force_and_force_only_replaces_exports() -> TestResult {
    let work = tempfile::tempdir()?;
    let db = demo_db(work.path())?;
    let out = work.path().join("record");
    fs::create_dir(&out)?;
    fs::write(out.join("mine.txt"), "keep me")?;
    let db_arg = path_arg(&db)?;
    let out_arg = path_arg(&out)?;

    let refused = teamctx(&["export", "--format", "okf", out_arg, "--db", db_arg])?;
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        text(&refused.stderr).contains("not empty"),
        "{}",
        text(&refused.stderr)
    );
    assert!(text(&refused.stdout).is_empty());
    assert_eq!(read_tree(&out)?.len(), 1, "nothing was written");

    let forced = teamctx(&[
        "export", "--format", "okf", out_arg, "--db", db_arg, "--force",
    ])?;
    assert!(forced.status.success(), "{}", text(&forced.stderr));
    let first = read_tree(&out)?;
    assert_eq!(first.len(), 79);
    assert_eq!(first["mine.txt"], "keep me");

    // A stale entry file from an older export goes; running again changes nothing.
    fs::write(out.join("decisions").join("D-999-stale.md"), "old")?;
    let again = teamctx(&[
        "export", "--format", "okf", out_arg, "--db", db_arg, "--force",
    ])?;
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert_eq!(read_tree(&out)?, first);
    Ok(())
}

#[test]
fn usage_and_environment_errors_are_clear() -> TestResult {
    let work = tempfile::tempdir()?;
    let out = work.path().join("record");
    let out_arg = path_arg(&out)?;

    let missing_format = teamctx(&["export", out_arg])?;
    assert_eq!(missing_format.status.code(), Some(2));
    assert!(text(&missing_format.stderr).contains("--format"));

    let bad_format = teamctx(&["export", "--format", "json", out_arg])?;
    assert_eq!(bad_format.status.code(), Some(2));

    let no_db = work.path().join("missing.db");
    let missing = teamctx(&[
        "export",
        "--format",
        "md",
        out_arg,
        "--db",
        path_arg(&no_db)?,
    ])?;
    assert_eq!(missing.status.code(), Some(2));
    assert!(text(&missing.stderr).contains("no database"));
    assert!(!out.exists(), "nothing was created");

    let help = teamctx(&["--help"])?;
    assert!(text(&help.stdout).contains("export --format okf|md"));
    Ok(())
}
