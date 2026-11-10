//! The host: `teamctx run`, `teamctx doctor --live` and `teamctx mcp --stdio` (plan
//! sections 5.2, 7, 10.1-10.3 and 12).
//!
//! `run` checks everything it can before touching the network: configuration (flags >
//! environment > file > defaults), the timezone, the bot token's presence, `secrets.env`
//! permissions and settings this build cannot honour. Then it opens the store, spawns
//! the single writer, serves `/mcp`, `/api/v1` and `/healthz`, and starts the Discord
//! connector with the interaction service (slash commands and context menus, registered
//! as guild commands on `READY`). Ctrl-C or SIGTERM stops HTTP, closes the gateway,
//! lets interactions in flight finish, drains the writer and checkpoints the WAL, then
//! exits 0.
//!
//! The wiring ([`serve_host`]) is generic over the gateway and the REST client, so the
//! end-to-end test runs it in-process against fakes.
//!
//! Nothing here prints or logs the bot token, the access tokens or chat text. Errors are
//! fixed text and name configuration fields, never their values.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teamctx_core::id::Snowflake;
use teamctx_core::repo::{IngestWrite, RepoError, RepoResult};
use teamctx_core::time::UnixMillis;
use teamctx_core::when::{FixedOffset, Zone};
use teamctx_discord::connector::{Clock, Connector, StatusSnapshot, Timing};
use teamctx_discord::doctor::{self, DoctorReport, MessageContentIntent};
use teamctx_discord::gateway::{Gateway, TwilightGateway};
use teamctx_discord::http::TwilightRest;
use teamctx_discord::interactions::{InteractionApi, InteractionConfig, InteractionService};
use teamctx_discord::secret::BotToken;
use teamctx_discord::store::HostStore;
use teamctx_discord::sync::{DiscordRest, SyncConfig};
use teamctx_server::auth::StoreTokens;
use teamctx_server::mcp::McpConfig;
use teamctx_server::status::{HostStatus, StatusProvider};
use teamctx_server::{HttpConfig, router, serve};
use teamctx_store::{DateResolver, Store, Writer, WriterClient};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::{self, Config, ConfigError, OfflineOptions};
use crate::search::parse_timezone;

/// The database file when `--db` is not given: the same default as `teamctx token`.
const DEFAULT_DB: &str = "teamctx.db";

/// How long HTTP may take to finish in-flight requests at shutdown.
const HTTP_STOP: Duration = Duration::from_secs(10);

/// How long the connector may take to close the gateway and drain at shutdown.
const CONNECTOR_STOP: Duration = Duration::from_secs(30);

/// Interactions queued between the gateway loop and the interaction service.
const INTERACTION_QUEUE: usize = 64;

/// Retention runs at start and then once a day (plan section 6.1).
const RETENTION_EVERY: Duration = Duration::from_secs(24 * 3600);

const DAY_MS: i64 = 86_400_000;

// ---------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------

/// Options of `teamctx run` and `teamctx doctor --live`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RunOptions {
    pub(crate) config: OfflineOptions,
    pub(crate) db: Option<PathBuf>,
}

fn usage(field: &'static str, reason: &'static str) -> ConfigError {
    ConfigError {
        field,
        reason,
        line: None,
    }
}

/// Removes a flag without a value from `args`; returns whether it was there.
pub(crate) fn take_flag(args: &mut Vec<OsString>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|arg| arg.to_str() != Some(flag));
    args.len() != before
}

/// Parses `run [--config PATH] [--db PATH] [config overrides]`.
pub(crate) fn parse_run(
    args: impl IntoIterator<Item = OsString>,
) -> Result<RunOptions, ConfigError> {
    let mut rest = Vec::new();
    let mut db = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg.to_str() == Some("--db") {
            db = Some(PathBuf::from(
                args.next()
                    .ok_or_else(|| usage("--db", "needs a file path"))?,
            ));
        } else {
            rest.push(arg);
        }
    }
    let config = config::parse_cli(rest)?;
    if config.application_id.is_some() {
        return Err(usage("--application-id", "is only used by init"));
    }
    Ok(RunOptions { config, db })
}

/// Options of `teamctx mcp --stdio`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct McpOptions {
    db: PathBuf,
    utc_offset_seconds: i32,
}

/// Parses `mcp --stdio [--db PATH] [--timezone TZ]`.
pub(crate) fn parse_mcp(
    args: impl IntoIterator<Item = OsString>,
) -> Result<McpOptions, ConfigError> {
    let mut stdio = false;
    let mut db = None;
    let mut offset = 0;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--stdio") => stdio = true,
            Some("--db") => {
                db = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| usage("--db", "needs a file path"))?,
                ));
            }
            Some("--timezone") => {
                let value = args
                    .next()
                    .ok_or_else(|| usage("--timezone", "needs a value"))?;
                offset = value
                    .to_str()
                    .and_then(|text| parse_timezone(text).ok())
                    .ok_or_else(|| usage("--timezone", TIMEZONE_HELP))?
                    .seconds();
            }
            _ => {
                return Err(usage(
                    "mcp",
                    "unknown option; use mcp --stdio [--db PATH] [--timezone TZ]",
                ));
            }
        }
    }
    if !stdio {
        return Err(usage(
            "mcp",
            "only --stdio is supported here; remote agents use the /mcp endpoint of `teamctx run`",
        ));
    }
    Ok(McpOptions {
        db: db.unwrap_or_else(|| PathBuf::from(DEFAULT_DB)),
        utc_offset_seconds: offset,
    })
}

/// Options of `teamctx serve`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ServeOptions {
    db: PathBuf,
    bind: SocketAddr,
    utc_offset_seconds: i32,
}

/// Parses `serve [--db PATH] [--bind IP:PORT] [--timezone TZ]`.
pub(crate) fn parse_serve(
    args: impl IntoIterator<Item = OsString>,
) -> Result<ServeOptions, ConfigError> {
    let mut db = None;
    let mut bind = None;
    let mut offset = 0;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--db") => {
                db = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| usage("--db", "needs a file path"))?,
                ));
            }
            Some("--bind") => {
                bind = Some(
                    args.next()
                        .and_then(|value| value.to_str().and_then(|text| text.parse().ok()))
                        .ok_or_else(|| {
                            usage("--bind", "must be IP:PORT, such as 127.0.0.1:7420")
                        })?,
                );
            }
            Some("--timezone") => {
                let value = args
                    .next()
                    .ok_or_else(|| usage("--timezone", "needs a value"))?;
                offset = value
                    .to_str()
                    .and_then(|text| parse_timezone(text).ok())
                    .ok_or_else(|| usage("--timezone", TIMEZONE_HELP))?
                    .seconds();
            }
            _ => {
                return Err(usage(
                    "serve",
                    "unknown option; use serve [--db PATH] [--bind IP:PORT] [--timezone TZ]",
                ));
            }
        }
    }
    Ok(ServeOptions {
        db: db.unwrap_or_else(|| PathBuf::from(DEFAULT_DB)),
        bind: bind.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 7420))),
        utc_offset_seconds: offset,
    })
}

const TIMEZONE_HELP: &str = "must be UTC or a fixed offset such as +02:00; named zones \
                             (America/Toronto) need a time zone database this build does not have";

// ---------------------------------------------------------------------------------------
// Validated host settings
// ---------------------------------------------------------------------------------------

/// Why the host would not start or stopped badly. Fixed text, no values.
#[derive(Debug)]
pub(crate) enum RunError {
    Config(ConfigError),
    MissingToken,
    SecretsFile,
    Store,
    Bind,
    Runtime,
    Tls,
    Http,
    Shutdown,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => fmt::Display::fmt(error, f),
            Self::MissingToken => f.write_str(
                "the environment variable named by discord.token_env is not set or empty; \
                 see docs/guides/discord-bot.md",
            ),
            Self::SecretsFile => f.write_str(
                "secrets.env must be an owner-only regular file (Unix mode 0600); refusing to start",
            ),
            Self::Store => f.write_str("cannot open or migrate the database"),
            Self::Bind => f.write_str("cannot listen on server.bind (address in use or not allowed)"),
            Self::Runtime => f.write_str("cannot start the async runtime"),
            Self::Tls => f.write_str("cannot set up TLS for Discord"),
            Self::Http => f.write_str("the HTTP server stopped unexpectedly"),
            Self::Shutdown => f.write_str(
                "shutdown did not complete cleanly (the writer could not drain or checkpoint)",
            ),
        }
    }
}

impl From<ConfigError> for RunError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

/// Everything `run` needs from the configuration, checked.
#[derive(Debug)]
pub(crate) struct Settings {
    pub(crate) bind: SocketAddr,
    pub(crate) allowed_hosts: Vec<String>,
    pub(crate) utc_offset_seconds: i32,
    pub(crate) sync: SyncConfig,
    pub(crate) retention_days: u32,
    pub(crate) public_bind_without_url: bool,
    pub(crate) llm_configured: bool,
}

/// Checks the parts of the configuration only `run` needs.
pub(crate) fn settings(config: &Config) -> Result<Settings, ConfigError> {
    let guild = config::snowflake(&config.discord.guild_id, "discord.guild_id")?;
    let guild =
        Snowflake::new(guild.get()).ok_or_else(|| usage("discord.guild_id", "must not be zero"))?;
    let bind: SocketAddr = config
        .server
        .bind
        .parse()
        .map_err(|_| usage("server.bind", "must be an IP address and port"))?;
    let utc_offset_seconds = parse_timezone(&config.server.timezone)
        .map_err(|_| usage("server.timezone", TIMEZONE_HELP))?
        .seconds();
    if config.discord.reaction_capture.enabled {
        return Err(usage(
            "discord.reaction_capture.enabled",
            "reaction capture is not built yet; set it to false",
        ));
    }
    let mut sync = SyncConfig::new(guild, "Discord server");
    sync.include_channels
        .clone_from(&config.discord.include_channels);
    sync.exclude_channels
        .clone_from(&config.discord.exclude_channels);
    sync.backfill_days = config.sync.backfill_days;
    sync.recheck_window = Duration::from_secs(u64::from(config.sync.recheck_window_hours) * 3600);
    Ok(Settings {
        bind,
        allowed_hosts: allowed_hosts(bind, config.server.public_url.as_deref()),
        utc_offset_seconds,
        sync,
        retention_days: config.retention.max_age_days,
        public_bind_without_url: !bind.ip().is_loopback() && config.server.public_url.is_none(),
        llm_configured: config.llm.is_some(),
    })
}

/// `Host` values `/mcp` accepts (DNS-rebinding protection): the loopback names for a
/// loopback or wildcard bind, the bound address otherwise, and the host of
/// `server.public_url`. Entries without a port match any port.
pub(crate) fn allowed_hosts(bind: SocketAddr, public_url: Option<&str>) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    let ip = bind.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        hosts.extend(["localhost", "127.0.0.1", "[::1]"].map(str::to_owned));
    } else {
        hosts.push(match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        });
    }
    if let Some(authority) = public_url.and_then(url_authority) {
        hosts.push(authority);
    }
    hosts.dedup();
    hosts
}

/// The lowercase `host[:port]` of an `http(s)://` URL (config validation already
/// rejected credentials, whitespace and other schemes).
fn url_authority(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty() && !authority.contains('@')).then(|| authority.to_ascii_lowercase())
}

/// The bot token from the variable named by `discord.token_env`.
fn bot_token(config: &Config) -> Result<BotToken, RunError> {
    std::env::var(&config.discord.token_env)
        .ok()
        .and_then(BotToken::new)
        .ok_or(RunError::MissingToken)
}

// ---------------------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------------------

/// What [`serve_host`] runs: production passes twilight, tests pass fakes.
pub(crate) struct HostParts<G, R> {
    pub(crate) db: PathBuf,
    pub(crate) listener: TcpListener,
    pub(crate) gateway: G,
    pub(crate) rest: Arc<R>,
    pub(crate) settings: Settings,
    pub(crate) clock: Clock,
    pub(crate) timing: Timing,
}

fn host_status(snapshot: &StatusSnapshot) -> HostStatus {
    HostStatus {
        source: snapshot.source,
        gateway: snapshot.gateway.as_str(),
        last_event_at: snapshot.last_event_at,
        events: snapshot.events,
        disconnects: snapshot.disconnects,
        last_sync_started_at: snapshot.last_sync_started_at,
        last_sync_finished_at: snapshot.last_sync_finished_at,
        last_sync_ok: snapshot.last_sync_ok,
        sync_runs: snapshot.sync_runs,
        problem: snapshot.problem,
    }
}

/// Runs the host until `shutdown` fires, then stops it in the plan's order: stop HTTP,
/// close the gateway and drain the ingest queue, drain the writer and checkpoint the WAL.
pub(crate) async fn serve_host<G, R>(
    parts: HostParts<G, R>,
    shutdown: CancellationToken,
) -> Result<(), RunError>
where
    G: Gateway + 'static,
    R: DiscordRest + InteractionApi + Send + Sync + 'static,
{
    let HostParts {
        db,
        listener,
        gateway,
        rest,
        settings,
        clock,
        timing,
    } = parts;
    let read_only = || Store::open_read_only(&db).map_err(|_| RunError::Store);
    let writer = Writer::spawn(Store::open(&db).map_err(|_| RunError::Store)?)
        .map_err(|_| RunError::Store)?;
    let zone = FixedOffset::from_seconds(settings.utc_offset_seconds)
        .ok_or_else(|| usage("server.timezone", TIMEZONE_HELP))?;
    let (interactions, interaction_events) = tokio::sync::mpsc::channel(INTERACTION_QUEUE);

    let connector = {
        let client = writer.client();
        let path = db.clone();
        Connector::new(
            gateway,
            Arc::clone(&rest),
            move || -> RepoResult<HostStore> {
                Ok(HostStore::new(
                    client.clone(),
                    Store::open_read_only(&path)?,
                ))
            },
            settings.sync.clone(),
            Arc::clone(&clock),
        )
        .with_timing(timing)
        .with_interactions(interactions)
    };
    let status = connector.status();
    // Slash commands and context menus (plan section 7.3): answers go through the same
    // REST client, record writes through the single writer, reads through their own
    // read-only connection with dates in `server.timezone`.
    let service = {
        let mut reader = read_only()?;
        reader.set_date_resolver(midnight_in(zone));
        InteractionService::new(
            Arc::clone(&rest),
            HostStore::new(writer.client(), reader),
            InteractionConfig {
                guild: settings.sync.guild,
                zone,
            },
            status.clone(),
            Arc::clone(&clock),
        )
    };
    let provider: StatusProvider = Arc::new(move || host_status(&status.snapshot()));
    let mcp = McpConfig::new(read_only()?)
        .with_clock(Arc::clone(&clock))
        .with_utc_offset_seconds(settings.utc_offset_seconds)
        .with_status(provider);
    let http_stop = CancellationToken::new();
    let app = router(HttpConfig {
        tokens: Arc::new(StoreTokens::new(read_only()?, writer.client())),
        allowed_hosts: settings.allowed_hosts.clone(),
        shutdown: http_stop.clone(),
        mcp,
    });
    let mut http = tokio::spawn(serve(listener, app, http_stop.clone()));
    let connector_stop = CancellationToken::new();
    let mut connector = tokio::spawn(connector.run(connector_stop.clone()));
    let service_stop = CancellationToken::new();
    let mut service = tokio::spawn(service.run(interaction_events, service_stop.clone()));
    let retention = (settings.retention_days > 0).then(|| {
        tokio::spawn(retention_loop(
            writer.client(),
            settings.retention_days,
            Arc::clone(&clock),
        ))
    });
    tracing::info!("teamctx is running");

    let mut http_failed = false;
    let mut connector_done = false;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            result = &mut http => {
                tracing::error!(ok = matches!(result, Ok(Ok(()))), "the HTTP server stopped");
                http_failed = true;
                break;
            }
            result = &mut connector, if !connector_done => {
                connector_done = true;
                match result {
                    Ok(Ok(())) => {}
                    // The record stays readable over HTTP; /api/v1/status shows the problem.
                    Ok(Err(error)) => tracing::error!(%error, "the Discord connector stopped; still serving the record"),
                    Err(_) => tracing::error!("the Discord connector task failed"),
                }
            }
        }
    }

    tracing::info!("shutting down");
    http_stop.cancel();
    if !http_failed && tokio::time::timeout(HTTP_STOP, &mut http).await.is_err() {
        tracing::warn!("HTTP requests did not finish in time");
        http.abort();
    }
    connector_stop.cancel();
    if !connector_done
        && tokio::time::timeout(CONNECTOR_STOP, &mut connector)
            .await
            .is_err()
    {
        tracing::warn!("the Discord connector did not stop in time");
        connector.abort();
    }
    // Interactions already being answered finish (bounded inside the service) before the
    // writer drains.
    service_stop.cancel();
    if tokio::time::timeout(CONNECTOR_STOP, &mut service)
        .await
        .is_err()
    {
        tracing::warn!("the interaction service did not stop in time");
        service.abort();
    }
    if let Some(retention) = retention {
        retention.abort();
    }
    let drained = tokio::task::spawn_blocking(move || writer.shutdown())
        .await
        .map_err(|_| RunError::Shutdown)?;
    if drained.is_err() {
        return Err(RunError::Shutdown);
    }
    tracing::info!("stopped; writer drained and WAL checkpointed");
    if http_failed {
        return Err(RunError::Http);
    }
    Ok(())
}

/// Midnight of a calendar date in `zone`, for `before:`/`after:` in `/search`.
fn midnight_in(zone: FixedOffset) -> DateResolver {
    Arc::new(move |date| {
        zone.start_of_day(date)
            .map_err(|_| RepoError::NotFound { what: "date" })
    })
}

/// Deletes messages older than `days` (plan section 6.1). Returns how many went.
pub(crate) fn purge_once(writer: &WriterClient, days: u32, now: UnixMillis) -> RepoResult<u64> {
    let cutoff = UnixMillis::from_millis(now.as_millis().saturating_sub(i64::from(days) * DAY_MS));
    writer.clone().purge_messages_before(cutoff, now)
}

async fn retention_loop(writer: WriterClient, days: u32, clock: Clock) {
    loop {
        let now = clock();
        let client = writer.clone();
        match tokio::task::spawn_blocking(move || purge_once(&client, days, now)).await {
            Ok(Ok(purged)) => tracing::info!(purged, "retention ran"),
            _ => tracing::warn!("retention could not run"),
        }
        tokio::time::sleep(RETENTION_EVERY).await;
    }
}

fn system_now() -> UnixMillis {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    UnixMillis::from_millis(millis)
}

/// Waits for Ctrl-C, or SIGTERM on Unix. If signals cannot be installed it waits forever
/// rather than shutting down at once.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        term.recv().await;
                    }
                }
                _ = term.recv() => {}
            }
            return;
        }
    }
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Logs to stderr at INFO, without colour codes. Stdout stays free (it is the MCP
/// channel for `mcp --stdio`). Nothing logged by teamctx carries a secret or chat text.
fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------

/// Everything `run` checks before it touches the network or the database.
fn prepare(options: &RunOptions) -> Result<(Settings, BotToken), RunError> {
    let config = config::load_options(&options.config, |key| std::env::var(key).ok())?;
    let settings = settings(&config)?;
    if !config::check_secrets_file(std::path::Path::new("secrets.env"))? {
        return Err(RunError::SecretsFile);
    }
    let token = bot_token(&config)?;
    Ok((settings, token))
}

/// `teamctx run`.
pub(crate) fn run(options: &RunOptions, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let (settings, token) = match prepare(options) {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return 1;
        }
    };
    if settings.public_bind_without_url {
        let _ = writeln!(
            err,
            "warning: server.bind is not loopback and server.public_url is not set; put \
             TLS (Tailscale, a tunnel or a reverse proxy) in front before teammates connect"
        );
    }
    if settings.llm_configured {
        let _ = writeln!(
            err,
            "note: [llm] is configured, but /ask is not served by this build yet"
        );
    }
    let db = options
        .db
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DB));
    let _ = writeln!(
        out,
        "teamctx run: serving on {} with database {}; press Ctrl-C to stop",
        settings.bind,
        db.display()
    );
    let _ = out.flush();
    init_logging();
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        let _ = writeln!(err, "error: {}", RunError::Runtime);
        return 1;
    };
    let shutdown = CancellationToken::new();
    let result = runtime.block_on(async {
        let gateway = TwilightGateway::new(&token).map_err(|_| RunError::Tls)?;
        let rest = TwilightRest::new(&token, tokio::runtime::Handle::current(), shutdown.clone())
            .map_err(|_| RunError::Tls)?;
        drop(token);
        let listener = TcpListener::bind(settings.bind)
            .await
            .map_err(|_| RunError::Bind)?;
        let stop = shutdown.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            stop.cancel();
        });
        serve_host(
            HostParts {
                db,
                listener,
                gateway,
                rest: Arc::new(rest),
                settings,
                clock: Arc::new(system_now),
                timing: Timing::default(),
            },
            shutdown,
        )
        .await
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            1
        }
    }
}

/// `teamctx doctor --live`: the offline checks, then read-only Discord checks.
pub(crate) fn doctor_live(
    options: &OfflineOptions,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    let offline_ok = match config::execute(false, options, out) {
        Ok(ok) => ok,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return 1;
        }
    };
    let config = match config::load_options(options, |key| std::env::var(key).ok()) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return 1;
        }
    };
    let (Ok(guild), Ok(token)) = (
        config::snowflake(&config.discord.guild_id, "discord.guild_id"),
        bot_token(&config),
    ) else {
        let _ = writeln!(err, "error: {}", RunError::MissingToken);
        return 1;
    };
    let Some(guild) = Snowflake::new(guild.get()) else {
        return 1;
    };
    let _ = writeln!(out, "\nLive checks (read-only requests to Discord):");
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        let _ = writeln!(err, "error: {}", RunError::Runtime);
        return 1;
    };
    let result = runtime.block_on(async {
        let rest = TwilightRest::new(
            &token,
            tokio::runtime::Handle::current(),
            CancellationToken::new(),
        )
        .map_err(|_| None)?;
        doctor::check(&rest, guild).await.map_err(Some)
    });
    match result {
        Ok(report) => {
            let ready = print_live_report(&report, out).is_ok() && report.ready();
            u8::from(!(ready && offline_ok))
        }
        Err(error) => {
            match error {
                Some(error) => {
                    let _ = writeln!(err, "error: {error}");
                }
                None => {
                    let _ = writeln!(err, "error: {}", RunError::Tls);
                }
            }
            1
        }
    }
}

/// Prints the live report. Channel names are untrusted: `{:?}` escapes control
/// characters so a name cannot rewrite the terminal.
pub(crate) fn print_live_report(
    report: &DoctorReport,
    out: &mut impl Write,
) -> std::io::Result<()> {
    writeln!(out, "Bot user id: {}", report.bot_user)?;
    writeln!(out, "Guild: visible to the bot")?;
    match report.message_content {
        MessageContentIntent::Enabled => writeln!(out, "Message Content intent: enabled")?,
        MessageContentIntent::Disabled => writeln!(
            out,
            "Message Content intent: NOT enabled; enable it on the Bot page of the Developer Portal"
        )?,
    }
    if report.administrator {
        writeln!(
            out,
            "Warning: the bot has Administrator or owns the server; it needs only View Channels and Read Message History (66560)"
        )?;
    }
    let readable = report.channels.iter().filter(|c| c.readable).count();
    writeln!(
        out,
        "Channels: {readable} of {} text, announcement and forum channels readable",
        report.channels.len()
    )?;
    for channel in &report.channels {
        let state = if channel.readable {
            "readable"
        } else {
            "NOT readable"
        };
        write!(out, "  {} {:?}: {state}", channel.id, channel.name)?;
        if channel.extra != 0 && !report.administrator {
            write!(
                out,
                "; extra permissions {:#x} (only 66560 is needed)",
                channel.extra
            )?;
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        "Not checked: private-thread membership, and whether the gateway delivers content (run shows close code 4014 if not)."
    )?;
    writeln!(
        out,
        "Live checks: {}",
        if report.ready() { "passed" } else { "FAILED" }
    )
}

/// `teamctx serve`: the record over HTTP from an existing database, with no Discord
/// connection. For trying the MCP server, `/api/v1` and `teamctx q` on the demo record
/// (or a copy of a real one); `run` is the production host.
pub(crate) fn serve_only(options: &ServeOptions, out: &mut impl Write, err: &mut impl Write) -> u8 {
    if !options.bind.ip().is_loopback() {
        let _ = writeln!(
            err,
            "error: serve only binds loopback addresses; use `teamctx run` for a team host"
        );
        return 1;
    }
    if !options.db.is_file() {
        let _ = writeln!(
            err,
            "error: no database at {:?}; run `teamctx demo --db PATH` first, or pass --db PATH",
            options.db
        );
        return 1;
    }
    init_logging();
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        let _ = writeln!(err, "error: {}", RunError::Runtime);
        return 1;
    };
    let result = runtime.block_on(async {
        let db = options.db.as_path();
        let writer = Writer::spawn(Store::open(db).map_err(|_| RunError::Store)?)
            .map_err(|_| RunError::Store)?;
        let read_only = || Store::open_read_only(db).map_err(|_| RunError::Store);
        let zone = FixedOffset::from_seconds(options.utc_offset_seconds)
            .ok_or_else(|| usage("--timezone", TIMEZONE_HELP))?;
        let mut mcp_store = read_only()?;
        mcp_store.set_date_resolver(midnight_in(zone));
        let mcp = McpConfig::new(mcp_store).with_utc_offset_seconds(options.utc_offset_seconds);
        let stop = CancellationToken::new();
        let app = router(HttpConfig {
            tokens: Arc::new(StoreTokens::new(read_only()?, writer.client())),
            allowed_hosts: allowed_hosts(options.bind, None),
            shutdown: stop.clone(),
            mcp,
        });
        let listener = TcpListener::bind(options.bind)
            .await
            .map_err(|_| RunError::Bind)?;
        let _ = writeln!(
            out,
            "teamctx serve: http://{} (no Discord connection) with database {}; press Ctrl-C to stop",
            options.bind,
            db.display()
        );
        let _ = out.flush();
        let signal_stop = stop.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            signal_stop.cancel();
        });
        let served = serve(listener, app, stop).await;
        let drained = tokio::task::spawn_blocking(move || writer.shutdown())
            .await
            .map_err(|_| RunError::Shutdown)?;
        if served.is_err() {
            return Err(RunError::Http);
        }
        drained.map_err(|_| RunError::Shutdown)
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            1
        }
    }
}

/// `teamctx mcp --stdio`.
pub(crate) fn mcp_stdio(options: &McpOptions, err: &mut impl Write) -> u8 {
    if !options.db.is_file() {
        let _ = writeln!(
            err,
            "error: no database at {:?}; pass --db PATH",
            options.db
        );
        return 1;
    }
    init_logging();
    let Ok(store) = Store::open_read_only(&options.db) else {
        let _ = writeln!(err, "error: {}", RunError::Store);
        return 1;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        let _ = writeln!(err, "error: {}", RunError::Runtime);
        return 1;
    };
    let config = McpConfig::new(store).with_utc_offset_seconds(options.utc_offset_seconds);
    let result = runtime.block_on(teamctx_server::stdio::serve_stdio(
        config,
        CancellationToken::new(),
    ));
    // The stdin bridge thread may still be blocked on a read; do not wait for it.
    runtime.shutdown_background();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests;
