# Security Policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub private vulnerability reporting:

<https://github.com/koolestasian/discord-team-context/security/advisories/new>

Do not open a public issue, pull request or discussion for a vulnerability.

Include the affected version or commit, the steps to reproduce, and the impact as you see
it. Do not send real bot tokens, access tokens, API keys, chat messages or personal data.
Describe what you found and we will agree how to share the rest.

## Supported versions

No release has been made yet, so no version is supported. After v0.1.0, only the latest
release receives security fixes.

## What to expect

teamctx is maintained by one person, part-time, on a best-effort basis. We aim to
acknowledge a report within 7 days. We do not promise a fixed timeline for a fix or for
disclosure. We will keep you informed while we work on it.

## Scope

In scope, for example:

- Exposure of the bot token, personal access tokens or LLM keys, in logs, responses,
  web pages or error messages.
- Prompt injection that makes the program or an AI agent act on chat content, such as
  calling tools, revealing data or writing anywhere it should not.
- Any write to Discord beyond replies to interactions.
- Authentication or authorisation bypass on the MCP server or the web view.
- Code that lets someone read or change a team's record without access.

Out of scope: vulnerabilities in Discord, LLM providers or other third-party services.
Report those to the provider.

The threat model is planned for milestone M8 (`docs/design/threat-model.md`). Until then,
[plan §13](docs/design/implementation-plan.md#13-security-and-privacy) summarises the
threats the design addresses.
