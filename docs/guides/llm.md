# Optional language model setup

`teamctx run` can answer `/ask question`, summarise a catch-up with
`/catchup summarize:true`, and serve `POST /api/v1/ask`. Without an `[llm]` section,
the Discord command and summary option are not registered and HTTP `/ask` returns 503.
`teamctx serve` and `mcp --stdio` do not enable a model.

This wiring is tested with a fake provider only. Real Discord delivery and one hosted
and one local model still need manual verification before M6 is complete.

## Hosted provider

Add this to the configuration used by `run`, replacing the model placeholder with an
actual model id available on your account:

```toml
[llm]
provider = "anthropic"
base_url = "https://api.anthropic.com"
model = "<your-model-id>"
api_key_env = "TEAMCTX_LLM_KEY"
max_questions_per_member_per_hour = 10
max_tokens_per_day = 200000
```

Set the key in the named environment variable before starting the host. Do not put
the key in TOML, a command argument or a committed file. `run` refuses missing keys,
placeholder model names and malformed URLs before contacting Discord or opening the
database. It never follows provider redirects, and sending a key over plain HTTP is
allowed only to loopback. Use `openai_compatible` with the provider's base URL including
its `/v1` path for Chat Completions providers.

## Local provider (examples, not live verified)

Start your local server and load a chat model. For LM Studio, use:

```toml
[llm]
provider = "openai_compatible"
base_url = "http://127.0.0.1:1234/v1"
model = "<your-loaded-model-id>"
api_key_env = "TEAMCTX_LOCAL_LLM_KEY"
max_questions_per_member_per_hour = 10
max_tokens_per_day = 200000
```

For local Ollama, use `http://127.0.0.1:11434/v1` and the exact installed model name.
Replace the placeholder: `run` rejects angle brackets in a model id. A loopback
OpenAI-compatible server can run without a key; leave the named variable unset when
authentication is disabled. If authentication is enabled, set it to the server's key.
A present but empty or invalid key is refused.

These endpoints follow the [LM Studio Chat Completions documentation](https://lmstudio.ai/docs/developer/openai-compat/chat-completions)
and [Ollama compatibility documentation](https://docs.ollama.com/api/openai-compatibility),
checked 2026-11-10. They are setup examples, not evidence of a successful local call.

## What the model receives

One request contains fixed system rules, your question, and a bounded bundle of record
entries and messages with author, channel and date. Bot messages and excluded channels
are left out. Discord retrieval is limited to channels the asking person can read;
public replies use only channels everyone can read. Existing private-thread membership
and permission-cache limitations in the Discord guide also apply here.

The model has no tools. Citations are checked against the supplied bundle; fabricated
citations are removed. Answers without a valid citation become a fixed "not found"
reply with top search hits. Every URL is removed from answer text; separate citations
contain only numeric Discord jump links. Citation validation establishes source ids,
not that the model's claims correctly describe those sources. Always check them.

The optional model-based query expansion in plan section 11 is deferred: no retrieval
eval demonstrates a gain yet. Existing keyword retrieval remains in use.

Hourly question and rolling 24-hour token budgets persist in SQLite. The limits apply
per Discord member, or per HTTP access token. Tokens are not linked to members in
v0.1, so two tokens and a Discord account have separate budgets. A call reserves its
estimated prompt plus output allowance atomically; failed calls count toward the
question limit. A cancelled call keeps its reservation charge. At most 16 model calls
run at once across both transports; calls time out after 60 seconds. Errors are fixed
text and never include provider bodies, credentials or the question.

## HTTP endpoint

Send a bearer token created with `teamctx token create` and JSON:

```http
POST /api/v1/ask
Authorization: Bearer <your-token>
Content-Type: application/json

{"question":"When do we ship the beta?"}
```

Only `question` is accepted (at most 500 characters; body at most 4096 bytes). There is
no client-selected member or channel scope: a token can read the whole included record,
as over MCP. The response has `found`, `notice`, framed `text`, and `citations` with
`id` and nullable `url`. Treat the framed text as untrusted data; a browser must display
it as text, never HTML. Responses use `Cache-Control: no-store`.

Errors: 400 invalid input, 401 missing/invalid/revoked bearer token, 413 oversized body,
415 wrong content type, 429 exhausted budget, 502 provider failure, 503 disabled or
unavailable host. The existing auth-failure limiter can also return 429.

## Manual acceptance checks still required

On a throwaway Discord server, enable `[llm]` and verify `/ask` registration, immediate
ephemeral deferral, the edited answer and jump links, a "not found" reply, budget
refusal and `/catchup summarize:true`. Restart without `[llm]` and verify those options
disappear. Run the same source-backed question with one hosted and one local model;
check citations and usage accounting. Never put real chat, keys or server ids in test
fixtures or reports committed to this repository.
