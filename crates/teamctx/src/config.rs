//! Configuration without secret values. Precedence: explicit CLI fields, named
//! TEAMCTX_* environment overrides, TOML file, then defaults. Validation errors never
//! echo source values (a mistakenly pasted credential must not reach a terminal).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU64;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) server: Server,
    pub(crate) discord: Discord,
    pub(crate) sync: Sync,
    pub(crate) retention: Retention,
    pub(crate) llm: Option<Llm>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Server {
    pub(crate) bind: String,
    pub(crate) public_url: Option<String>,
    pub(crate) timezone: String,
}
impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7420".to_owned(),
            public_url: None,
            timezone: "UTC".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Discord {
    pub(crate) guild_id: String,
    pub(crate) token_env: String,
    pub(crate) include_channels: Vec<String>,
    pub(crate) exclude_channels: Vec<String>,
    pub(crate) reaction_capture: ReactionCapture,
}
impl Default for Discord {
    fn default() -> Self {
        Self {
            guild_id: String::new(),
            token_env: "TEAMCTX_DISCORD_TOKEN".to_owned(),
            include_channels: Vec::new(),
            exclude_channels: Vec::new(),
            reaction_capture: ReactionCapture::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReactionCapture {
    pub(crate) enabled: bool,
    pub(crate) decision: String,
}
impl Default for ReactionCapture {
    fn default() -> Self {
        Self {
            enabled: false,
            decision: "\u{1F4CC}".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Sync {
    pub(crate) backfill_days: u32,
    pub(crate) recheck_window_hours: u32,
}
impl Default for Sync {
    fn default() -> Self {
        Self {
            backfill_days: 90,
            recheck_window_hours: 48,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Retention {
    pub(crate) max_age_days: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Llm {
    pub(crate) provider: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key_env: String,
    pub(crate) max_questions_per_member_per_hour: u32,
    pub(crate) max_tokens_per_day: u64,
}
impl Default for Llm {
    fn default() -> Self {
        Self {
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            api_key_env: "TEAMCTX_LLM_KEY".to_owned(),
            max_questions_per_member_per_hour: 10,
            max_tokens_per_day: 200_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigError {
    pub(crate) field: &'static str,
    pub(crate) reason: &'static str,
    pub(crate) line: Option<usize>,
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.field)?;
        if let Some(line) = self.line {
            write!(f, " at line {line}")?;
        }
        write!(f, ": {}", self.reason)
    }
}
impl std::error::Error for ConfigError {}
fn invalid(field: &'static str, reason: &'static str) -> ConfigError {
    ConfigError {
        field,
        reason,
        line: None,
    }
}

/// Documented environment overrides. Arrays are comma-separated; empty means no items.
/// Token/key environment variables contain credentials, not config overrides, and are
/// intentionally absent from this list.
const OVERRIDES: &[(&str, &str, &str)] = &[
    ("TEAMCTX_BIND", "server.bind", "string"),
    ("TEAMCTX_PUBLIC_URL", "server.public_url", "string"),
    ("TEAMCTX_TIMEZONE", "server.timezone", "string"),
    ("TEAMCTX_GUILD_ID", "discord.guild_id", "string"),
    ("TEAMCTX_TOKEN_ENV", "discord.token_env", "string"),
    (
        "TEAMCTX_INCLUDE_CHANNELS",
        "discord.include_channels",
        "array",
    ),
    (
        "TEAMCTX_EXCLUDE_CHANNELS",
        "discord.exclude_channels",
        "array",
    ),
    (
        "TEAMCTX_REACTION_CAPTURE",
        "discord.reaction_capture.enabled",
        "bool",
    ),
    (
        "TEAMCTX_DECISION_EMOJI",
        "discord.reaction_capture.decision",
        "string",
    ),
    ("TEAMCTX_BACKFILL_DAYS", "sync.backfill_days", "integer"),
    (
        "TEAMCTX_RECHECK_WINDOW_HOURS",
        "sync.recheck_window_hours",
        "integer",
    ),
    (
        "TEAMCTX_RETENTION_MAX_AGE_DAYS",
        "retention.max_age_days",
        "integer",
    ),
    ("TEAMCTX_LLM_PROVIDER", "llm.provider", "string"),
    ("TEAMCTX_LLM_BASE_URL", "llm.base_url", "string"),
    ("TEAMCTX_LLM_MODEL", "llm.model", "string"),
    ("TEAMCTX_LLM_KEY_ENV", "llm.api_key_env", "string"),
    (
        "TEAMCTX_LLM_MAX_QUESTIONS_PER_MEMBER_PER_HOUR",
        "llm.max_questions_per_member_per_hour",
        "integer",
    ),
    (
        "TEAMCTX_LLM_MAX_TOKENS_PER_DAY",
        "llm.max_tokens_per_day",
        "integer",
    ),
];

/// CLI overrides use the same names as the environment table, but take precedence.
/// No environment mutation occurs, so tests and concurrent callers remain independent.
pub(crate) fn load(
    file: &str,
    env: impl Fn(&str) -> Option<String>,
    cli: &BTreeMap<String, String>,
) -> Result<Config, ConfigError> {
    let mut root: toml::Table = toml::from_str(file).map_err(|_| {
        invalid(
            "teamctx.toml",
            "invalid TOML syntax; check the configuration file",
        )
    })?;
    // Validate the file's shape before overlaying: overrides may not conceal typos.
    let _: Config = toml::from_str(file).map_err(|error: toml::de::Error| {
        let mut result = invalid(
            "teamctx.toml",
            "unknown field or incorrect value type; secrets belong in environment variables",
        );
        result.line = error.span().map(|span| {
            file.as_bytes()[..span.start.min(file.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1
        });
        result
    })?;
    for &(name, field, kind) in OVERRIDES {
        if let Some(raw) = cli.get(name).cloned().or_else(|| env(name)) {
            let value = match kind {
                "bool" => toml::Value::Boolean(
                    raw.parse()
                        .map_err(|_| invalid(name, "must be true or false"))?,
                ),
                "integer" => toml::Value::Integer(
                    raw.parse()
                        .map_err(|_| invalid(name, "must be a nonnegative integer"))?,
                ),
                "array" => toml::Value::Array(
                    raw.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| toml::Value::String(s.to_owned()))
                        .collect(),
                ),
                _ => toml::Value::String(raw),
            };
            let mut parts = field.split('.').peekable();
            let mut table = &mut root;
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    table.insert(part.to_owned(), value);
                    break;
                }
                table = table
                    .entry(part)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| invalid(name, "conflicts with a configuration section"))?;
            }
        }
    }
    let config: Config = toml::Value::Table(root).try_into().map_err(|_| {
        invalid(
            "configuration",
            "incorrect value type or numeric value out of range",
        )
    })?;
    config.validate()?;
    Ok(config)
}

pub(crate) fn snowflake(value: &str, field: &'static str) -> Result<NonZeroU64, ConfigError> {
    if value.starts_with('0') || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(
            field,
            "must be a positive decimal Discord id without leading zeros",
        ));
    }
    value
        .parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(|| invalid(field, "Discord id is out of range"))
}

fn env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// Only a conservative absolute HTTP(S) shape check. The actual HTTP client validates
// full URL syntax when wired in; no custom URL parser or new URL dependency is added.
fn http_url(value: &str) -> bool {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"));
    rest.is_some_and(|s| {
        let authority = s.split(['/', '?', '#']).next().unwrap_or("");
        !authority.is_empty()
            && !authority.contains('@')
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
    })
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.server.bind.parse::<SocketAddr>().is_err() {
            return Err(invalid(
                "server.bind",
                "must be an IP address and port, for example 127.0.0.1:7420",
            ));
        }
        if self.server.timezone.trim().is_empty()
            || self.server.timezone.chars().any(char::is_control)
        {
            return Err(invalid(
                "server.timezone",
                "must name a timezone, for example UTC or America/Toronto",
            ));
        }
        if self
            .server
            .public_url
            .as_deref()
            .is_some_and(|s| !http_url(s))
        {
            return Err(invalid(
                "server.public_url",
                "must be an absolute HTTP(S) URL without credentials or whitespace",
            ));
        }
        snowflake(&self.discord.guild_id, "discord.guild_id")?;
        if !env_name(&self.discord.token_env) {
            return Err(invalid(
                "discord.token_env",
                "must name an environment variable, not contain the bot token",
            ));
        }
        if self
            .discord
            .include_channels
            .iter()
            .chain(&self.discord.exclude_channels)
            .any(|s| s.trim().is_empty() || s.chars().any(char::is_control))
        {
            return Err(invalid(
                "discord.include_channels/exclude_channels",
                "channel names must not be empty or contain control characters",
            ));
        }
        if self.discord.reaction_capture.enabled
            && self.discord.reaction_capture.decision.trim().is_empty()
        {
            return Err(invalid(
                "discord.reaction_capture.decision",
                "must be set when capture is enabled",
            ));
        }
        if self.sync.recheck_window_hours == 0 {
            return Err(invalid(
                "sync.recheck_window_hours",
                "must be greater than zero",
            ));
        }
        if let Some(llm) = &self.llm {
            if !matches!(llm.provider.as_str(), "anthropic" | "openai_compatible") {
                return Err(invalid(
                    "llm.provider",
                    "must be anthropic or openai_compatible",
                ));
            }
            if !http_url(&llm.base_url) {
                return Err(invalid(
                    "llm.base_url",
                    "must be an absolute HTTP(S) URL without credentials or whitespace",
                ));
            }
            if llm.model.trim().is_empty() {
                return Err(invalid(
                    "llm.model",
                    "must be set when LLM support is enabled",
                ));
            }
            if !env_name(&llm.api_key_env) {
                return Err(invalid(
                    "llm.api_key_env",
                    "must name an environment variable",
                ));
            }
            if llm.max_questions_per_member_per_hour == 0 || llm.max_tokens_per_day == 0 {
                return Err(invalid(
                    "llm",
                    "question and token limits must be greater than zero",
                ));
            }
        }
        Ok(())
    }

    /// Denylist wins; an empty allowlist includes every channel the bot can see.
    pub(crate) fn includes_channel(&self, name: &str) -> bool {
        !self.discord.exclude_channels.iter().any(|s| s == name)
            && (self.discord.include_channels.is_empty()
                || self.discord.include_channels.iter().any(|s| s == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> &'static str {
        "[discord]\nguild_id = '123'\n"
    }

    #[test]
    fn precedence_and_defaults() {
        let cli = BTreeMap::from([("TEAMCTX_BIND".to_owned(), "127.0.0.1:9000".to_owned())]);
        let env = |key: &str| {
            if key == "TEAMCTX_BIND" {
                Some("127.0.0.1:8000".to_owned())
            } else {
                None
            }
        };
        let config = load(file(), env, &cli).unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:9000");
        assert_eq!(
            load(file(), env, &BTreeMap::new()).unwrap().server.bind,
            "127.0.0.1:8000"
        );
        let config = load(file(), |_| None, &BTreeMap::new()).unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:7420");
        assert_eq!(config.sync.backfill_days, 90);
        assert_eq!(config.sync.recheck_window_hours, 48);
        assert_eq!(config.discord.reaction_capture.decision, "\u{1F4CC}");
        assert_eq!(config.discord.reaction_capture.decision.chars().count(), 1);
        assert!(config.llm.is_none());
        assert_eq!(config.retention.max_age_days, 0);
    }

    #[test]
    fn errors_do_not_echo_credentials() {
        for text in [
            "[discord]\nguild_id='fake-secret'",
            "token='fake-secret'",
            "[discord]\nguild_id='123'\ntoken='fake-secret'",
            "[discord]\nguild_id = fake-secret",
        ] {
            let error = load(text, |_| None, &BTreeMap::new()).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("fake-secret"));
        }
        let error = load(
            file(),
            |key| (key == "TEAMCTX_BIND").then(|| "fake-secret".to_owned()),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.field, "server.bind");
        assert!(!error.to_string().contains("fake-secret"));
    }

    #[test]
    fn channel_selection_and_scalar_validation() {
        let config = load("[discord]\nguild_id='123'\ninclude_channels=['dev','random']\nexclude_channels=['random']", |_| None, &BTreeMap::new()).unwrap();
        assert!(config.includes_channel("dev"));
        assert!(!config.includes_channel("random"));
        assert!(!config.includes_channel("other"));
        for (name, value) in [
            ("TEAMCTX_GUILD_ID", "0"),
            ("TEAMCTX_GUILD_ID", "+123"),
            ("TEAMCTX_RECHECK_WINDOW_HOURS", "0"),
            ("TEAMCTX_BACKFILL_DAYS", "-1"),
            ("TEAMCTX_REACTION_CAPTURE", "yes"),
            ("TEAMCTX_TOKEN_ENV", "not a name"),
            ("TEAMCTX_PUBLIC_URL", "https://user:password@example.com"),
        ] {
            assert!(
                load(
                    file(),
                    |key| (key == name).then(|| value.to_owned()),
                    &BTreeMap::new()
                )
                .is_err()
            );
        }
        assert!(load("[discord]\nguild_id='123'\n[llm]\nprovider='anthropic'\nbase_url='https://api.anthropic.com'\nmodel='example-model'", |_| None, &BTreeMap::new()).is_ok());
    }

    #[test]
    fn secrets_file_is_regular_and_owner_only_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_secrets_file(&dir.path().join("absent")).unwrap());
        assert!(!check_secrets_file(dir.path()).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let path = dir.path().join("secrets.env");
            std::fs::write(&path, "FICTIONAL_TOKEN=not-a-credential").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!check_secrets_file(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(check_secrets_file(&path).unwrap());
            let link = dir.path().join("link.env");
            symlink(&path, &link).unwrap();
            assert!(!check_secrets_file(&link).unwrap());
        }
    }
}

/// Offline init/doctor flags; credentials are deliberately not accepted as arguments.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct OfflineOptions {
    pub(crate) config_path: Option<std::path::PathBuf>,
    pub(crate) application_id: Option<NonZeroU64>,
    pub(crate) overrides: BTreeMap<String, String>,
}

pub(crate) fn parse_cli(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<OfflineOptions, ConfigError> {
    let mut options = OfflineOptions::default();
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let name = match flag.to_str() {
            Some("--config") => {
                options.config_path = Some(
                    args.next()
                        .ok_or_else(|| invalid("--config", "needs a file path"))?
                        .into(),
                );
                continue;
            }
            Some("--application-id") => {
                let value = args
                    .next()
                    .ok_or_else(|| invalid("--application-id", "needs the application id"))?;
                options.application_id = Some(snowflake(
                    value
                        .to_str()
                        .ok_or_else(|| invalid("--application-id", "must be decimal text"))?,
                    "--application-id",
                )?);
                continue;
            }
            Some("--bind") => "TEAMCTX_BIND",
            Some("--guild-id") => "TEAMCTX_GUILD_ID",
            Some("--timezone") => "TEAMCTX_TIMEZONE",
            Some("--public-url") => "TEAMCTX_PUBLIC_URL",
            Some("--token-env") => "TEAMCTX_TOKEN_ENV",
            Some("--include-channels") => "TEAMCTX_INCLUDE_CHANNELS",
            Some("--exclude-channels") => "TEAMCTX_EXCLUDE_CHANNELS",
            Some("--backfill-days") => "TEAMCTX_BACKFILL_DAYS",
            Some("--recheck-window-hours") => "TEAMCTX_RECHECK_WINDOW_HOURS",
            _ => {
                return Err(invalid(
                    "arguments",
                    "unknown offline command option; see teamctx --help",
                ));
            }
        };
        let value = args
            .next()
            .ok_or_else(|| invalid(name, "option needs a value"))?;
        options.overrides.insert(
            name.to_owned(),
            value
                .into_string()
                .map_err(|_| invalid(name, "value must be UTF-8 text"))?,
        );
    }
    Ok(options)
}

/// Prints a configuration template or an explicitly offline diagnostic. No network
/// calls, token values, API responses, environment dumps or file writes occur here.
pub(crate) fn execute(
    init: bool,
    options: &OfflineOptions,
    out: &mut impl std::io::Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let path = options
        .config_path
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("teamctx.toml"));
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && options.config_path.is_none() =>
        {
            String::new()
        }
        Err(_) => {
            return Err(invalid("--config", "cannot read the configuration file as UTF-8").into());
        }
    };
    let config = load(&text, |key| std::env::var(key).ok(), &options.overrides)?;
    if init {
        let rendered = toml::to_string_pretty(&config)
            .map_err(|_| invalid("configuration", "cannot render the configuration template"))?;
        writeln!(
            out,
            "# Offline template. Review and save as teamctx.toml; no file was written."
        )?;
        writeln!(
            out,
            "# LLM off unless [llm] is configured. When enabled, retrieved chat excerpts are sent to the configured provider."
        )?;
        if let Some(application_id) = options.application_id {
            let guild = snowflake(&config.discord.guild_id, "discord.guild_id")?;
            writeln!(
                out,
                "# Invite (open manually after team agreement): {}",
                teamctx_discord::permissions::invite_url(application_id, guild)
            )?;
        } else {
            writeln!(
                out,
                "# Add --application-id to print an invite link. See docs/guides/discord-bot.md."
            )?;
        }
        write!(out, "{rendered}")?;
        return Ok(true);
    }
    writeln!(out, "Offline configuration checks passed.")?;
    // Only inspect whether a credential is present, never retain or format its value.
    let token_set = std::env::var_os(&config.discord.token_env).is_some_and(|v| !v.is_empty());
    writeln!(
        out,
        "Discord token environment variable: {}",
        if token_set { "set" } else { "missing" }
    )?;
    let bind: SocketAddr = config
        .server
        .bind
        .parse()
        .map_err(|_| invalid("server.bind", "invalid address"))?;
    if !bind.ip().is_loopback() {
        writeln!(
            out,
            "Warning: server.bind is not loopback; configure authenticated private access before serving."
        )?;
    }
    writeln!(
        out,
        "LLM support: {}",
        if config.llm.is_some() {
            "configured; provider and key not checked"
        } else {
            "disabled"
        }
    )?;
    let included_names = config
        .discord
        .include_channels
        .iter()
        .filter(|name| config.includes_channel(name))
        .count();
    writeln!(
        out,
        "Channel selection: denylist overrides allowlist ({} included names, {} excluded names).",
        included_names,
        config.discord.exclude_channels.len()
    )?;
    let permissions_ok = check_secrets_file(std::path::Path::new("secrets.env"))?;
    #[cfg(not(unix))]
    writeln!(
        out,
        "Not checked: Windows ACLs for secrets.env; restrict access to the host account."
    )?;
    if !permissions_ok {
        writeln!(
            out,
            "secrets.env must be an owner-only regular file with Unix mode 0600."
        )?;
    }
    writeln!(
        out,
        "Not checked: live token validity, guild access, effective channel permissions, private-thread membership, timezone database and full URL parsing."
    )?;
    writeln!(
        out,
        "This build does not load secrets.env automatically. Use process environment variables. Live init/doctor checks arrive after the Discord test gate."
    )?;
    Ok(token_set && permissions_ok)
}

fn check_secrets_file(path: &std::path::Path) -> Result<bool, ConfigError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(_) => return Err(invalid("secrets.env", "cannot inspect file permissions")),
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(metadata.permissions().mode() & 0o777 == 0o600)
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    } // Windows ACL validation is unavailable through std; reported separately.
}
