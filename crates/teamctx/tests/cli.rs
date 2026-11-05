//! End-to-end checks of the `teamctx` binary: version, help and usage errors.

use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn teamctx(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .args(args)
        .output()
}

fn stdout(output: &Output) -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(output.stdout.clone())
}

fn stderr(output: &Output) -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(output.stderr.clone())
}

#[test]
fn version_flags_print_name_and_version() -> TestResult {
    let expected = format!("teamctx {}\n", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        let output = teamctx(&[flag])?;
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        assert_eq!(stdout(&output)?, expected, "{flag}");
        assert!(output.stderr.is_empty(), "{flag}");
    }
    Ok(())
}

#[test]
fn help_flags_describe_a_pre_alpha_build_with_no_commands() -> TestResult {
    for flag in ["--help", "-h"] {
        let output = teamctx(&[flag])?;
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        let text = stdout(&output)?;
        assert!(text.contains("pre-alpha"), "{flag}: {text}");
        assert!(text.contains("Usage: teamctx"), "{flag}: {text}");
        assert!(!text.contains("Commands:"), "{flag}: {text}");
        assert!(output.stderr.is_empty(), "{flag}");
    }
    Ok(())
}

#[test]
fn unknown_argument_is_a_usage_error() -> TestResult {
    for args in [&["serve"][..], &["--bogus"], &["--version", "extra"]] {
        let output = teamctx(args)?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let message = stderr(&output)?;
        assert!(message.starts_with("error: "), "{args:?}: {message}");
        assert!(message.contains("teamctx --help"), "{args:?}: {message}");
    }
    Ok(())
}
