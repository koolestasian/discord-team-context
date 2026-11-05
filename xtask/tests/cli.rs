//! Checks of the `xtask` binary: help output and rejection of unknown tasks.

use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn xtask(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .output()
}

#[test]
fn no_arguments_and_help_list_the_tasks() -> TestResult {
    for args in [&[][..], &["help"], &["--help"]] {
        let output = xtask(args)?;
        assert!(output.status.success(), "{args:?}: {:?}", output.status);
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("Usage: cargo xtask"), "{args:?}: {text}");
        assert!(text.contains("help"), "{args:?}: {text}");
        assert!(text.contains("M2"), "{args:?}: {text}");
    }
    Ok(())
}

#[test]
fn unknown_task_exits_with_code_two() -> TestResult {
    for args in [&["eval"][..], &["nope"], &["help", "extra"]] {
        let output = xtask(args)?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let message = String::from_utf8(output.stderr)?;
        assert!(
            message.starts_with("error: unknown task"),
            "{args:?}: {message}"
        );
    }
    Ok(())
}
