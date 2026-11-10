//! The `run`, `doctor --live` and `mcp --stdio` commands as a process.
//!
//! None of these tests reaches Discord: every `run` and `doctor --live` case is refused
//! by a check that happens before any network call, which is what they prove. The bot
//! token is fake and must never appear in the output. `mcp --stdio` runs for real on the
//! fictional demo database.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BOT_TOKEN: &str = "FAKE-discord-bot-token.run-cli.0000-not-a-real-credential";

fn teamctx(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_teamctx"));
    command.current_dir(dir).env_clear().args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("run teamctx")
}

fn all(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Arguments, environment, and the config field the error must name.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);

#[test]
fn run_refuses_bad_settings_before_any_network_call() {
    let dir = tempfile::tempdir().expect("temp dir");
    let token = [("TEAMCTX_DISCORD_TOKEN", BOT_TOKEN)];
    let cases: [Case<'_>; 5] = [
        (&["run", "--guild-id", "123"], &[], "discord.token_env"),
        (
            &["run", "--guild-id", "123", "--timezone", "America/Toronto"],
            &token,
            "server.timezone",
        ),
        (
            &["run", "--guild-id", "123"],
            &[
                ("TEAMCTX_DISCORD_TOKEN", BOT_TOKEN),
                ("TEAMCTX_REACTION_CAPTURE", "true"),
            ],
            "discord.reaction_capture.enabled",
        ),
        (&["run"], &token, "discord.guild_id"),
        (
            &["run", "--guild-id", "123", "--bind", BOT_TOKEN],
            &token,
            "server.bind",
        ),
    ];
    for (args, env, field) in cases {
        let output = teamctx(dir.path(), args, env);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = all(&output);
        assert!(text.contains(field), "{args:?}: {text}");
        assert!(!text.contains(BOT_TOKEN), "{args:?} echoed the token");
        assert!(
            !dir.path().join("teamctx.db").exists(),
            "{args:?} created a database"
        );
    }
}

#[cfg(unix)]
#[test]
fn run_refuses_a_group_readable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("temp dir");
    let secrets = dir.path().join("secrets.env");
    std::fs::write(&secrets, "FICTIONAL=not-a-credential\n").expect("write");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    let output = teamctx(
        dir.path(),
        &["run", "--guild-id", "123"],
        &[("TEAMCTX_DISCORD_TOKEN", BOT_TOKEN)],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(all(&output).contains("secrets.env"));
    assert!(!all(&output).contains(BOT_TOKEN));
}

#[test]
fn live_doctor_needs_a_token_and_never_prints_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = teamctx(dir.path(), &["doctor", "--live", "--guild-id", "123"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(all(&output).contains("discord.token_env"));
    let bad = teamctx(
        dir.path(),
        &["doctor", "--live", "--guild-id", BOT_TOKEN],
        &[("TEAMCTX_DISCORD_TOKEN", BOT_TOKEN)],
    );
    assert_ne!(bad.status.code(), Some(0));
    assert!(!all(&bad).contains(BOT_TOKEN));
}

#[test]
fn mcp_needs_stdio_and_an_existing_database() {
    let dir = tempfile::tempdir().expect("temp dir");
    let usage = teamctx(dir.path(), &["mcp"], &[]);
    assert_eq!(usage.status.code(), Some(2));
    let missing = teamctx(dir.path(), &["mcp", "--stdio", "--db", "absent.db"], &[]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        !dir.path().join("absent.db").exists(),
        "no database is created"
    );
}

#[test]
fn mcp_stdio_answers_on_stdout_and_logs_only_to_stderr() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("demo.db");
    let db_text = db.to_str().expect("utf-8 path");
    let demo = teamctx(dir.path(), &["demo", "--db", db_text], &[]);
    assert!(demo.status.success(), "{}", all(&demo));

    let mut child = Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .current_dir(dir.path())
        .env_clear()
        .args(["mcp", "--stdio", "--db", db_text])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    let mut send = |text: &str| {
        stdin.write_all(text.as_bytes()).expect("write");
        stdin.write_all(b"\n").expect("write");
        stdin.flush().expect("flush");
    };
    send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cli-test","version":"0"}}}"#,
    );
    let init: serde_json::Value =
        serde_json::from_str(&lines.next().expect("a line").expect("read")).expect("json");
    assert_eq!(init["id"], 1);
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    let list: serde_json::Value =
        serde_json::from_str(&lines.next().expect("a line").expect("read")).expect("json");
    assert_eq!(list["result"]["tools"].as_array().expect("tools").len(), 5);
    drop(stdin);
    let status = child.wait().expect("wait");
    assert!(status.success(), "{status:?}");
    assert!(lines.next().is_none(), "nothing else on stdout");
}
