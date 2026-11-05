# 0007. Optional LLM, two adapters, no SDKs

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D7, §11, §12, §13

## Context

Only `/ask` and the optional `summarize:true` flag on `/catchup` need a language model
(plan §1, §7.3, M6). Everything else (search, the record, catch-up, MCP, the web view)
works without one. Sending team chat to a third-party provider is a privacy decision
the team must make for itself (§13). Teams use different providers, and some prefer
local models.

The previous agentic approach, an LLM loop with tools, widens the damage a prompt
injection in chat can do and makes cost unpredictable (§11).

## Decision

The LLM is optional and bring-your-own-key. Without an `[llm]` section, `/ask` is
disabled. Two adapters sit behind one `LlmProvider` trait:

- `AnthropicMessages`: `POST /v1/messages`.
- `OpenAiCompatible`: `POST {base_url}/chat/completions`, which covers OpenRouter,
  Ollama and LM Studio (§3 D7).

Both call plain HTTP with `reqwest` (rustls); there are no provider SDKs. Both read
`usage` for token accounting. Model names and keys come from config and nothing is
hardcoded.

The `/ask` pipeline is a single call without tool use (§11). Retrieval comes from the
no-LLM layer, the prompt marks sources as untrusted, citations that do not match a
provided source are stripped, and unsupported URLs are removed. Bot messages and
excluded channels are never sent. Per-member hourly limits and a daily token cap come
from `llm_usage`, requests time out at 60 seconds, and provider errors are never echoed
to Discord.

## Alternatives considered

- **Provider SDKs.** Convenience and typed responses, but extra dependencies (ADR
  0011), and they change independently of our small request/response needs.
- **One provider only.** Less code, but locks teams in and excludes local models.
- **A tool-using agent loop for `/ask`.** Can find better evidence on hard questions;
  a bounded search loop (at most 3 searches) stays a v0.2 candidate, to be compared on
  the same eval (ADR 0014).
- **A required LLM.** Simplest product story, but it would make the core record
  unusable for teams unwilling to send chat to a provider.

## Consequences

### Good

- A smaller injection surface and predictable cost per question.
- Teams control whether any data leaves the host and which provider gets it.

### Bad

- Two adapters to maintain and test; adapters may diverge in response edge cases.
- A single call with no tools may answer less well on hard questions.

### Follow-ups

- M6: test with one hosted and one local (Ollama) model; fake provider in CI, never a
  real API.
