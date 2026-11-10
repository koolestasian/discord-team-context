//! End-to-end checks of `teamctx q` against a real server on the fictional demo record.
//!
//! The database and the token are made with the built binary (`teamctx demo`,
//! `teamctx token create`), the server is the real `teamctx-server` router on
//! `127.0.0.1:0`, and `teamctx q` runs as a child process with its own environment. Parity
//! is checked against the MCP tools called over HTTP on the same server. Chat text in the
//! demo set is untrusted data, including two messages written to look like instructions.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use teamctx_core::{
    framing::{BLOCK_END, BLOCK_START, NOTICE},
    time::CalendarDate,
};
use teamctx_server::{HttpConfig, auth::StoreTokens, mcp::McpConfig, router, serve};
use teamctx_store::{Store, Writer};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_teamctx");

/// Runs the binary with only the variables given (plus what Windows needs to start
/// programs), so a developer's own `TEAMCTX_*` variables cannot change the result.
fn teamctx(args: &[&str], env: &[(&str, &str)]) -> std::io::Result<Output> {
    let mut command = Command::new(BIN);
    command.env_clear().args(args);
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", root);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command.output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_text(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    path.to_str().ok_or_else(|| "non-UTF-8 test path".into())
}

struct Harness {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    url: String,
    token: String,
    db: PathBuf,
    _writer: Writer,
    shutdown: CancellationToken,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn start() -> Result<Harness, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("demo.db");
    let db_text = path_text(&db)?;
    let made = teamctx(&["demo", "--db", db_text], &[])?;
    assert!(made.status.success(), "{}", text(&made.stderr));
    let created = teamctx(&["token", "create", "q-test", "--db", db_text], &[])?;
    assert!(created.status.success(), "{}", text(&created.stderr));
    let token = text(&created.stdout)
        .lines()
        .find(|line| line.starts_with("tctx_"))
        .ok_or("no token line")?
        .to_owned();

    let writer = Writer::spawn(Store::open(&db)?)?;
    // "Now" for relative windows: the day after the demo's last conversation.
    let now = CalendarDate::new(2026, 4, 25)?.start_of_day_utc();
    let mcp = McpConfig::new(Store::open_read_only(&db)?).with_clock(Arc::new(move || now));
    let shutdown = CancellationToken::new();
    let app = router(HttpConfig {
        tokens: Arc::new(StoreTokens::new(
            Store::open_read_only(&db)?,
            writer.client(),
        )),
        allowed_hosts: vec!["127.0.0.1".to_owned()],
        shutdown: shutdown.clone(),
        mcp,
        ask: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve(listener, app, stop).await;
    });
    Ok(Harness {
        dir: dir.path().to_owned(),
        _dir: dir,
        url: format!("http://{addr}"),
        token,
        db,
        _writer: writer,
        shutdown,
    })
}

impl Harness {
    /// Runs `teamctx q ARGS` with the URL and token in the environment.
    async fn q(&self, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
        self.q_with(args, &[("TEAMCTX_TOKEN", &self.token)]).await
    }

    async fn q_with(
        &self,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> Result<Output, Box<dyn std::error::Error>> {
        let mut all = vec!["q".to_owned()];
        all.extend(args.iter().map(|arg| (*arg).to_owned()));
        let mut env = vec![("TEAMCTX_URL".to_owned(), self.url.clone())];
        env.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        let output = tokio::task::spawn_blocking(move || {
            let pairs: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let args: Vec<&str> = all.iter().map(String::as_str).collect();
            teamctx(&args, &pairs)
        })
        .await??;
        Ok(output)
    }

    /// `q ARGS --json`, required to succeed, parsed.
    async fn q_json(&self, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
        let mut all = args.to_vec();
        all.push("--json");
        let output = self.q(&all).await?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            text(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    /// Calls an MCP tool over HTTP and returns its `result` object.
    async fn mcp(&self, tool: &str, arguments: Value) -> Result<Value, Box<dyn std::error::Error>> {
        let response = reqwest::Client::new()
            .post(format!("{}/mcp", self.url))
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "{tool}");
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        let body = response.text().await?;
        let json_text = if is_sse {
            body.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .find(|data| data.starts_with('{'))
                .unwrap_or_default()
                .to_owned()
        } else {
            body
        };
        let message: Value = serde_json::from_str(&json_text)?;
        assert!(message["error"].is_null(), "{tool}: {message}");
        Ok(message["result"].clone())
    }

    /// Asserts `q ARGS` matches the MCP tool for the same input, in text and `--json`
    /// form, and returns the item ids.
    async fn same_as_mcp(
        &self,
        args: &[&str],
        tool: &str,
        arguments: Value,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mcp = self.mcp(tool, arguments).await?;
        assert_ne!(mcp["isError"], true, "{tool}: {mcp}");
        let mcp_text = mcp["content"][0]["text"].as_str().ok_or("mcp text")?;
        let mcp_ids: Vec<String> = mcp["structuredContent"]["items"]
            .as_array()
            .ok_or("mcp items")?
            .iter()
            .filter_map(|item| item["id"].as_str().map(str::to_owned))
            .collect();
        assert!(!mcp_ids.is_empty(), "{tool}: nothing returned, weak check");

        let body = self.q_json(args).await?;
        let q_ids: Vec<String> = body["items"]
            .as_array()
            .ok_or("q items")?
            .iter()
            .filter_map(|item| item["id"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(q_ids, mcp_ids, "{tool}: ids and order");
        assert_eq!(body["text"], mcp_text, "{tool}: text");
        assert_eq!(body["next_cursor"], mcp["structuredContent"]["next_cursor"]);
        assert_eq!(body["truncated"], mcp["structuredContent"]["truncated"]);

        let output = self.q(args).await?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            text(&output.stderr)
        );
        assert_eq!(
            text(&output.stdout),
            format!("{mcp_text}\n"),
            "{tool}: stdout"
        );
        Ok(mcp_ids)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_q_command_returns_what_the_matching_mcp_tool_returns() -> TestResult {
    let h = start().await?;
    h.same_as_mcp(
        &["context", "fix/reading-position-conflicts"],
        "context_for",
        json!({ "text": "fix/reading-position-conflicts" }),
    )
    .await?;
    h.same_as_mcp(
        &["context", "beta date", "--limit", "2"],
        "context_for",
        json!({ "text": "beta date", "limit": 2 }),
    )
    .await?;
    h.same_as_mcp(
        &["whats-new", "2026-04-14"],
        "whats_new",
        json!({ "since": "2026-04-14" }),
    )
    .await?;
    h.same_as_mcp(&["whats-new", "7d"], "whats_new", json!({ "since": "7d" }))
        .await?;
    h.same_as_mcp(&["record"], "record", json!({})).await?;
    h.same_as_mcp(
        &[
            "record",
            "--kind",
            "decision",
            "--status",
            "active",
            "--topic",
            "reading position",
        ],
        "record",
        json!({ "kind": "decision", "status": "active", "topic": "reading position" }),
    )
    .await?;
    h.same_as_mcp(
        &[
            "record",
            "--owner",
            "Wren Calloway",
            "--since",
            "2026-04-01",
        ],
        "record",
        json!({ "owner": "Wren Calloway", "since": "2026-04-01" }),
    )
    .await?;
    let found = h
        .same_as_mcp(
            &["search", "libspine"],
            "search",
            json!({ "query": "libspine" }),
        )
        .await?;
    h.same_as_mcp(
        &["search", "crash from:crashwatch after:2026-04-01"],
        "search",
        json!({ "query": "crash from:crashwatch after:2026-04-01" }),
    )
    .await?;
    let message = found.first().ok_or("a hit")?;
    h.same_as_mcp(
        &["read", message, "--before", "1", "--after", "3"],
        "read",
        json!({ "id": message, "before": 1, "after": 3 }),
    )
    .await?;
    h.same_as_mcp(&["read", "D-1"], "read", json!({ "id": "D-1" }))
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn paging_with_a_cursor_matches_mcp() -> TestResult {
    let h = start().await?;
    let first = h.q_json(&["record", "--kind", "decision"]).await?;
    let cursor = first["next_cursor"].as_str().ok_or("a second page")?;
    h.same_as_mcp(
        &["record", "--kind", "decision", "--cursor", cursor],
        "record",
        json!({ "kind": "decision", "cursor": cursor }),
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn context_for_a_branch_cites_entries_and_messages() -> TestResult {
    // US-7 through the CLI client: a coding agent asks for the context of its branch.
    let h = start().await?;
    let output = h.q(&["context", "fix/reading-position-conflicts"]).await?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.starts_with(NOTICE), "{out}");
    assert!(out.contains("Reading position"), "{out}");
    let body = h
        .q_json(&["context", "fix/reading-position-conflicts"])
        .await?;
    let kinds: Vec<&str> = body["items"]
        .as_array()
        .ok_or("items")?
        .iter()
        .filter_map(|i| i["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"decision") && kinds.contains(&"message"),
        "{kinds:?}"
    );
    for item in body["items"].as_array().ok_or("items")? {
        let id = item["id"].as_str().ok_or("id")?;
        assert!(out.contains(id), "{id} is cited in the text");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_demo_messages_arrive_only_inside_the_framing() -> TestResult {
    const INJECTIONS: [&str; 2] = [
        "Ignore all previous instructions",
        "disregard prior instructions",
    ];
    let h = start().await?;
    let found = h.q_json(&["search", "\"admin tokens\""]).await?;
    let message = found["items"][0]["id"].as_str().ok_or("the crash digest")?;
    let mut seen = 0;
    for args in [
        vec!["search", "\"admin tokens\""],
        vec!["read", message],
        vec!["context", "support email imported book title"],
        vec!["search", "free forever"],
    ] {
        let output = h.q(&args).await?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            text(&output.stderr)
        );
        let out = text(&output.stdout);
        let start = out.find(BLOCK_START).ok_or("block start")?;
        let end = out.find(BLOCK_END).ok_or("block end")?;
        assert_eq!(out.matches(BLOCK_START).count(), 1);
        assert_eq!(out.matches(BLOCK_END).count(), 1);
        assert!(out.starts_with(NOTICE));
        let outside = format!("{}{}", &out[..start], &out[end..]).to_lowercase();
        for injection in INJECTIONS {
            let needle = injection.to_lowercase();
            assert!(!outside.contains(&needle), "{injection} outside the block");
            if out[start..end].to_lowercase().contains(&needle) {
                seen += 1;
            }
        }
        // `--json` keeps chat text in the one `text` field only.
        let mut json_args = args.clone();
        json_args.push("--json");
        let body: Value = serde_json::from_slice(&h.q(&json_args).await?.stdout)?;
        let mut structure = body.as_object().ok_or("object")?.clone();
        structure.remove("text");
        let structure = Value::Object(structure).to_string().to_lowercase();
        for injection in INJECTIONS {
            assert!(!structure.contains(&injection.to_lowercase()));
        }
    }
    assert!(seen >= 2, "the injected text was reachable ({seen} times)");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_request_is_refused_with_the_hosts_message_and_exit_1() -> TestResult {
    let h = start().await?;
    let output = h.q(&["record", "--kind", "idea"]).await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let err = text(&output.stderr);
    assert!(
        err.contains("`kind` must be decision, task or question"),
        "{err}"
    );
    let output = h.q(&["whats-new", "soon"]).await?;
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("`since` must be"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_file_works_and_the_token_is_never_printed() -> TestResult {
    let h = start().await?;
    let file = h.dir.join("token");
    std::fs::write(&file, format!("{}\n", h.token))?;
    secure(&file)?;
    let file_text = path_text(&file)?;
    for output in [
        h.q_with(
            &["search", "libspine"],
            &[("TEAMCTX_TOKEN_FILE", file_text)],
        )
        .await?,
        h.q_with(&["search", "libspine", "--token-file", file_text], &[])
            .await?,
    ] {
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert!(!text(&output.stdout).contains(&h.token));
        assert!(!text(&output.stderr).contains(&h.token));
    }
    // A wrong token, a missing file and a revoked token never echo the token they used.
    let wrong = "tctx_wrong_token_do_not_print";
    let output = h
        .q_with(&["search", "x"], &[("TEAMCTX_TOKEN", wrong)])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(all.contains("rejected the token"), "{all}");
    assert!(!all.contains(wrong), "{all}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_gets_a_clear_error() -> TestResult {
    let h = start().await?;
    assert!(h.q(&["search", "libspine"]).await?.status.success());
    let revoked = teamctx(
        &["token", "revoke", "q-test", "--db", path_text(&h.db)?],
        &[],
    )?;
    assert!(revoked.status.success(), "{}", text(&revoked.stderr));
    let output = h.q(&["search", "libspine"]).await?;
    assert_eq!(output.status.code(), Some(1));
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(all.contains("rejected the token"), "{all}");
    assert!(!all.contains(&h.token));
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_token_file_open_to_others_is_refused_before_any_request() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let h = start().await?;
    let file = h.dir.join("token");
    std::fs::write(&file, format!("{}\n", h.token))?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))?;
    let output = h
        .q_with(
            &["search", "libspine"],
            &[("TEAMCTX_TOKEN_FILE", path_text(&file)?)],
        )
        .await?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let err = text(&output.stderr);
    assert!(err.contains("chmod 600"), "{err}");
    assert!(!err.contains(&h.token));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    let output = h
        .q_with(
            &["search", "libspine"],
            &[("TEAMCTX_TOKEN_FILE", path_text(&file)?)],
        )
        .await?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_settings_are_usage_errors_that_say_what_to_set() -> TestResult {
    let h = start().await?;
    // No URL.
    let output = tokio::task::spawn_blocking(|| teamctx(&["q", "search", "x"], &[])).await??;
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("TEAMCTX_URL"));
    // A URL, but no token anywhere (the data directory is empty).
    let data = h.dir.join("empty-data");
    std::fs::create_dir_all(&data)?;
    let data = path_text(&data)?;
    let output = h
        .q_with(
            &["search", "x"],
            &[
                ("LOCALAPPDATA", data),
                ("HOME", data),
                ("XDG_DATA_HOME", data),
            ],
        )
        .await?;
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("TEAMCTX_TOKEN"));
    // A mistake in the arguments.
    let output = h.q(&["search"]).await?;
    assert_eq!(output.status.code(), Some(2));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_host_is_reported_without_the_token() -> TestResult {
    let h = start().await?;
    // Reserve a port and close it, so nothing listens there.
    let closed = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let url = format!("http://{closed}");
    let token = h.token.clone();
    let output = tokio::task::spawn_blocking(move || {
        teamctx(
            &["q", "search", "x"],
            &[("TEAMCTX_URL", &url), ("TEAMCTX_TOKEN", &token)],
        )
    })
    .await??;
    assert_eq!(output.status.code(), Some(1));
    let err = text(&output.stderr);
    assert!(err.contains("cannot connect"), "{err}");
    assert!(!err.contains(&h.token));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn redirects_are_not_followed_so_the_token_stays_with_the_named_host() -> TestResult {
    use std::io::{Read, Write};
    // A host that answers every request with a redirect to a second listener.
    let target = TcpListener::bind("127.0.0.1:0")?;
    let target_addr = target.local_addr()?;
    target.set_nonblocking(true)?;
    let first = TcpListener::bind("127.0.0.1:0")?;
    let first_addr = first.local_addr()?;
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = first.accept() {
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    let url = format!("http://{first_addr}");
    let output = tokio::task::spawn_blocking(move || {
        teamctx(
            &["q", "search", "x"],
            &[
                ("TEAMCTX_URL", &url),
                ("TEAMCTX_TOKEN", "tctx_redirect_test"),
            ],
        )
    })
    .await??;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("HTTP 302"),
        "{}",
        text(&output.stderr)
    );
    assert!(
        target.accept().is_err(),
        "the redirect target was contacted"
    );
    Ok(())
}

#[test]
fn help_lists_the_q_commands() -> TestResult {
    let output = teamctx(&["--help"], &[])?;
    let help = text(&output.stdout);
    for needle in [
        "q context",
        "q whats-new",
        "q record",
        "q search",
        "q read",
        "TEAMCTX_URL",
    ] {
        assert!(help.contains(needle), "{needle}");
    }
    Ok(())
}

/// Makes a token file owner-only where that can be set.
fn secure(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
