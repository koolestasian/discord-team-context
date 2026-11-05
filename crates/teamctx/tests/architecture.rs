//! Guards the crate dependency direction from plan section 5.1:
//!
//! ```text
//! teamctx -> {discord, server, llm} -> store -> core
//! ```
//!
//! It reads every `crates/*/Cargo.toml` and checks which workspace crates each one
//! depends on. Adding a crate without listing it in `ALLOWED` fails the test, so the rule
//! cannot be skipped by accident.

use std::fs;
use std::path::Path;

/// For each crate, the workspace crates it may depend on (any dependency table).
const ALLOWED: &[(&str, &[&str])] = &[
    ("teamctx-core", &[]),
    ("teamctx-store", &["teamctx-core"]),
    ("teamctx-discord", &["teamctx-store", "teamctx-core"]),
    ("teamctx-llm", &["teamctx-store", "teamctx-core"]),
    ("teamctx-server", &["teamctx-store", "teamctx-core"]),
    (
        "teamctx",
        &[
            "teamctx-discord",
            "teamctx-server",
            "teamctx-llm",
            "teamctx-store",
            "teamctx-core",
        ],
    ),
];

/// Names of all dependencies declared in a manifest, from `[dependencies]`,
/// `[dev-dependencies]`, `[build-dependencies]` and their `[target.*]` forms.
fn dependency_names(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependency_table = false;
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_end_matches(']');
            in_dependency_table = header.ends_with("dependencies");
            // `[dependencies.foo]` style: the name is part of the header.
            if let Some((_, name)) = header.split_once("dependencies.") {
                names.push(name.to_owned());
            }
        } else if in_dependency_table && !line.is_empty() && !line.starts_with('#') {
            // `foo = "1"`, `foo.workspace = true` or `foo = { ... }`: the name comes first.
            let name = line.split(['=', '.']).next().unwrap_or(line).trim();
            names.push(name.to_owned());
        }
    }
    names
}

#[test]
fn dependency_names_are_read_from_every_dependency_table() {
    let manifest = r#"
[package]
name = "demo"

[dependencies]
a.workspace = true
# b = "1"
c = { version = "1" }

[target.'cfg(unix)'.dev-dependencies]
d = "1"

[dependencies.e]
version = "1"

[lints]
workspace = true
"#;
    assert_eq!(dependency_names(manifest), ["a", "c", "d", "e"]);
}

#[test]
fn crates_depend_only_in_the_planned_direction() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives in crates/");

    let mut found = Vec::new();
    for entry in fs::read_dir(crates_dir).expect("crates/ should be readable") {
        let path = entry.expect("directory entry").path();
        let manifest_path = path.join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("crate directory name should be UTF-8")
            .to_owned();
        let manifest = fs::read_to_string(&manifest_path).expect("manifest should be readable");
        let dependencies = dependency_names(&manifest);

        let Some((_, allowed)) = ALLOWED.iter().find(|(n, _)| *n == name) else {
            panic!("crate {name} is not listed in ALLOWED; add it with its permitted dependencies");
        };
        for dependency in &dependencies {
            if dependency.starts_with("teamctx") {
                assert!(
                    allowed.contains(&dependency.as_str()),
                    "{name} must not depend on {dependency}; allowed: {allowed:?}"
                );
            }
        }
        // The core crate has no dependencies at all: no I/O, no async, nothing external.
        if name == "teamctx-core" {
            assert!(
                dependencies.is_empty(),
                "teamctx-core must have no dependencies, found {dependencies:?}"
            );
        }
        found.push(name);
    }

    for (name, _) in ALLOWED {
        assert!(
            found.iter().any(|f| f == name),
            "ALLOWED lists {name} but crates/{name} is missing"
        );
    }
}
