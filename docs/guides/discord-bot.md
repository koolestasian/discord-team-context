# Discord test bot setup

Use a throwaway server with fictional messages for the live spikes (G1). This build's
`init` and `doctor` commands run **offline**. They do not connect the bot, register
commands, validate a token with Discord, or establish that US-1 is complete.

## 1. Create a private test application

1. Create a throwaway Discord server. In Discord's Advanced settings, enable Developer
   Mode, then copy that server's ID. Keep real team history out of test fixtures.
2. Open the [Developer Portal](https://discord.com/developers/applications), create an
   application, and copy its Application ID from General Information.
3. On the Bot page, disable **Public Bot**, and enable **Message Content Intent**.
   Leave Presence and Server Members intents off; they are not required by this plan.
4. Create/reset the bot token and save it directly to a password manager. Do not paste
   it into this repository, chat, a command argument, a screenshot, or a committed file.
   The token is different from the Application ID and from the OAuth client secret.

The connector will request GUILDS, GUILD_MESSAGES and MESSAGE_CONTENT. Reaction capture
is off by default; enabling it later also needs GUILD_MESSAGE_REACTIONS. Privileged
intents must be enabled in the portal as well as requested by the connector.
See [Discord's gateway documentation](https://docs.discord.com/developers/events/gateway#privileged-intents).

## 2. Prepare a config and invite

Run this with your application/server IDs in place of the words in angle brackets:

```text
cargo run -p teamctx -- init --application-id <application-id> --guild-id <server-id>
```

The command prints a TOML template and a commented invite URL; it writes no files.
Save the template as `teamctx.toml` **outside the repository**, or use a local ignored
config path. It contains only environment variable names, never credential values.
The default bind is `127.0.0.1:7420`, timezone UTC, backfill 90 days, recheck window
48 hours, retention unlimited, reaction capture disabled and LLM support disabled.
Use `--timezone America/Toronto` (or your timezone) when generating the template.
Timezone database lookup and complete URL parsing are not implemented in the offline
checker yet; the generated default does not require either a public URL or an LLM.

Before opening the invite, agree with the server owner on what is copied and who can
access it. The link requests **bot** and **applications.commands** scopes and exactly
**View Channels (1024) + Read Message History (65536) = 66560** permissions. It is locked
to the selected test guild. Do not add Send Messages, Add Reactions, Administrator,
Manage Messages or moderation permissions. Interaction replies without Send Messages
are a live spike acceptance check, not something offline tests establish.

Open the URL yourself and authorize it in the throwaway server. Check the bot's roles
and each test channel: everyone permissions, all bot roles, channel role overwrites,
then a member-specific bot overwrite affect its effective access. A deny on one role
can be outweighed by an allow on another role. Threads inherit their parent channel's
read permissions; private-thread membership is an additional condition. Categories
sync overwrites rather than adding a second overwrite layer.
See [Discord permissions](https://docs.discord.com/developers/topics/permissions) and
[OAuth bot authorization](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow).

## 3. Set credentials without echoing them

PowerShell 7, in the terminal that will launch the live checks:

```powershell
$env:TEAMCTX_TEST_DISCORD_TOKEN = Read-Host 'Test bot token' -MaskInput
$env:TEAMCTX_TEST_GUILD_ID = Read-Host 'Test server ID'
$env:TEAMCTX_DISCORD_TOKEN = $env:TEAMCTX_TEST_DISCORD_TOKEN
```

In Windows PowerShell 5.1, use a secure prompt instead:

```powershell
$testBotSecret = Read-Host 'Test bot token' -AsSecureString
$testBotCredential = [pscredential]::new('test-bot', $testBotSecret)
$env:TEAMCTX_TEST_DISCORD_TOKEN = $testBotCredential.GetNetworkCredential().Password
$env:TEAMCTX_TEST_GUILD_ID = Read-Host 'Test server ID'
$env:TEAMCTX_DISCORD_TOKEN = $env:TEAMCTX_TEST_DISCORD_TOKEN
Remove-Variable testBotSecret, testBotCredential
```

For bash:

```bash
read -rs -p 'Test bot token: ' TEAMCTX_TEST_DISCORD_TOKEN; printf '\n'
export TEAMCTX_TEST_DISCORD_TOKEN
read -r -p 'Test server ID: ' TEAMCTX_TEST_GUILD_ID
export TEAMCTX_TEST_GUILD_ID
export TEAMCTX_DISCORD_TOKEN="$TEAMCTX_TEST_DISCORD_TOKEN"
```

These variables last only for that shell and its children. Launch the build/live test
session from that shell so it inherits them. Do not use a literal token in shell
commands or enable command tracing. A different already-running app cannot see newly
set process variables. The build gate checks TEST names; runtime config uses
`discord.token_env` (default TEAMCTX_DISCORD_TOKEN).

`secrets.env` is not loaded automatically in this build. If such a file exists, the
offline checker rejects symlinks/nonregular files and on Unix requires mode 0600.
On Windows it reports that ACL validation is not implemented; restrict access to the
host account. Environment-only setup avoids storing a token in a file.

## 4. Run the offline doctor and hand off G1

```text
cargo run -p teamctx -- doctor --config <path-to-teamctx.toml>
```

A zero exit means the offline config checks passed and a nonempty token variable is
present. Output never includes the token. It explicitly lists live checks that were
not performed. No server permissions or bot settings are changed by this command.
The live S1/S2 spikes still need to verify token validity, guild/channel/thread access,
edits/deletes and ephemeral interaction replies. Do not use this build with real team
history before those tests and the team privacy agreement.

If a token leaks, reset it on the Bot page, replace the process environment value and
restart the process using it. Remove the token from any local files; resetting it is
necessary even if a leaked copy was deleted.

## Configuration precedence

Explicit flags override the following environment variables, which override the TOML
file, which overrides defaults. `--config` selects the file; a missing explicit file is
an error. Without it, `teamctx.toml` is read only if present. Unknown fields are rejected.

| Environment variable | TOML field |
|---|---|
| TEAMCTX_BIND | server.bind |
| TEAMCTX_PUBLIC_URL | server.public_url |
| TEAMCTX_TIMEZONE | server.timezone |
| TEAMCTX_GUILD_ID | discord.guild_id |
| TEAMCTX_TOKEN_ENV | discord.token_env |
| TEAMCTX_INCLUDE_CHANNELS / TEAMCTX_EXCLUDE_CHANNELS | discord.include_channels / exclude_channels (comma-separated) |
| TEAMCTX_REACTION_CAPTURE / TEAMCTX_DECISION_EMOJI | discord.reaction_capture.enabled / decision |
| TEAMCTX_BACKFILL_DAYS / TEAMCTX_RECHECK_WINDOW_HOURS | sync.backfill_days / recheck_window_hours |
| TEAMCTX_RETENTION_MAX_AGE_DAYS | retention.max_age_days |
| TEAMCTX_LLM_PROVIDER / TEAMCTX_LLM_BASE_URL / TEAMCTX_LLM_MODEL | llm.provider / base_url / model |
| TEAMCTX_LLM_KEY_ENV | llm.api_key_env |
| TEAMCTX_LLM_MAX_QUESTIONS_PER_MEMBER_PER_HOUR | llm.max_questions_per_member_per_hour |
| TEAMCTX_LLM_MAX_TOKENS_PER_DAY | llm.max_tokens_per_day |

`--help` lists supported CLI overrides. IDs are positive decimal strings, arrays are
channel names without `#`, and the denylist wins over the allowlist. An empty allowlist
includes every supported channel the bot can read. LLM support is off when `[llm]` is
absent; enabling it sends retrieved chat excerpts to the configured provider and requires
explicit model/base URL configuration. No actual LLM calls are made by these commands.
