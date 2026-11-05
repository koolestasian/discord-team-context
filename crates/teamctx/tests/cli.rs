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
fn help_flags_describe_the_offline_pre_alpha_commands() -> TestResult {
    for flag in ["--help", "-h"] {
        let output = teamctx(&[flag])?;
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        let text = stdout(&output)?;
        assert!(text.contains("pre-alpha"), "{flag}: {text}");
        assert!(text.contains("Usage: teamctx"), "{flag}: {text}");
        assert!(text.contains("Offline commands:"), "{flag}: {text}");
        assert!(text.contains("makes no network calls"), "{flag}: {text}");
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

#[test]
fn offline_init_and_doctor_never_echo_tokens_or_claim_live_checks() -> TestResult {
    let dir = tempfile::tempdir()?;
    let run = |args: &[&str], token: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_teamctx"));
        command.current_dir(dir.path()).env_clear().args(args);
        if let Some(token) = token {
            command.env("TEAMCTX_DISCORD_TOKEN", token);
        }
        command.output()
    };
    let init = run(
        &["init", "--guild-id", "123", "--application-id", "456"],
        Some("fictional-test-secret"),
    )?;
    assert!(init.status.success(), "{}", stderr(&init)?);
    let template = stdout(&init)?;
    assert!(template.contains("permissions=66560"));
    assert!(!template.contains("fictional-test-secret"));
    let config: toml::Value = toml::from_str(&template)?;
    assert_eq!(config["discord"]["guild_id"].as_str(), Some("123"));
    assert!(!dir.path().join("teamctx.toml").exists());

    for (token, expected) in [(None, 1), (Some("fictional-test-secret"), 0)] {
        let doctor = run(&["doctor", "--guild-id", "123"], token)?;
        assert_eq!(doctor.status.code(), Some(expected));
        let output = stdout(&doctor)?;
        assert!(output.contains("Offline configuration checks passed"));
        assert!(output.contains("Not checked: live token validity"));
        assert!(!output.contains("fictional-test-secret"));
    }
    Ok(())
}

#[test]
fn invalid_config_points_to_field_without_echoing_its_value() -> TestResult {
    let dir = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .current_dir(dir.path())
        .env_clear()
        .args([
            "doctor",
            "--guild-id",
            "123",
            "--bind",
            "fictional-test-secret",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output)?.contains("server.bind"));
    assert!(!stderr(&output)?.contains("fictional-test-secret"));
    Ok(())
}
