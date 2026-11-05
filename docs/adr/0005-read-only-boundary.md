# 0005. Read-only boundary and interaction-only replies

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D5, §7.1, §7.3, §13

## Context

The bot reads a team's private conversations. Trust in it depends on a boundary that
is easy to state and easy to check: it never writes into the team's Discord. At the
same time, people need to use it from inside Discord, which means answering slash
commands and message context menus (plan §7.3). Replies to interactions are a separate
mechanism from posting messages: they answer a user's own command.

Non-goals (§1) rule out posting on its own, moderating, or writing anywhere else.

## Decision

The bot never creates, edits or deletes messages, adds reactions, or performs
moderation actions. Its only outbound writes are responses to interactions:

- Every response is ephemeral (flag 64) unless the user passes `public:true`.
- Commands that may take over two seconds (`/ask`, a large `/catchup`) defer first,
  then edit the original response.
- Reaction capture (a configured emoji such as a pin) is off by default and only
  reads reaction events; the `GUILD_MESSAGE_REACTIONS` intent is requested only when
  it is on (§7.1).

The invite URL requests only View Channels (1024) and Read Message History (65536),
which is permission integer `66560`, with scopes `bot` and `applications.commands`.
The app is not a Public Bot. `teamctx init` generates the URL; `teamctx doctor` checks
the effective permissions per channel and warns about extras.

The boundary is enforced in code and in CI: the plan specifies a CI grep test that
finds no message-create calls outside interaction responses (§13).

The assumption that a bot without Send Messages can still answer interactions is
validated by spike S2 in M1. If S2 fails, the permission set or this decision must be
revisited with the maintainer before M3.

## Alternatives considered

- **Request Send Messages and post digests or reminders.** More features (scheduled
  summaries), but it breaks the read-only promise, widens what a compromised token
  could do, and is a stated non-goal.
- **Reply to interactions publicly by default.** Simpler to follow in a channel, but
  it puts answers (possibly drawn from private channels) in front of everyone;
  ephemeral-by-default is the safer default, with `public:true` as an explicit choice.
- **No Discord-side commands, web and MCP only.** Smallest Discord footprint, but
  non-coders would lose the workflow where they already are (personas in §2).

## Consequences

### Good

- A short, checkable promise: minimal invite permissions, no write calls, CI test.
- A leaked bot token cannot be used to post or moderate, because the permissions do
  not allow it.

### Bad

- No proactive notifications (task reminders, digests) in v0.1.
- Users need normal permission to run commands in a channel; the bot's own
  permissions do not cover that.

### Follow-ups

- M1 spike S2; M3 `doctor` permission check with correct channel-overwrite
  resolution; CI grep test for write calls.
