# How teamctx was built

Why the project exists, how it was built and how it was tested. About a five-minute read.

teamctx was built with AI coding agents (Claude Code and Codex) under my direction: I wrote
the plan and the rules, agents wrote much of the code, and I reviewed, audited and tested it.

- [Where it started](#where-it-started)
- [Research before code](#research-before-code)
- [How the build ran](#how-the-build-ran)
- [What I learned](#what-i-learned)
- [Testing without a live server](#testing-without-a-live-server)
- [By the numbers](#by-the-numbers)
- [Status](#status)

## Where it started

My team coordinates in Discord. Like most small teams, we decide things there: which demo
to show, who owns the intro, what the deadline is. Then the decision scrolls away. Someone
asks again, or acts on the old answer, or reads 300 messages to catch up after a day off.

AI coding agents have the same problem from the other side. They know the code and nothing
about what the team decided.

At the start of November 2026 I built a private prototype: a shared read-only bot plus a
small MCP server so our agents could read the channels. An agent could answer "who is
presenting, and what is the deadline?" without anyone repeating it. The prototype also
showed what to change: one shared token for the whole team, a PC that had to stay on, a
Python install for every teammate, and an agent loop that was slow and hard to test.

So I started a clean repository, kept the lessons and none of the code or data (ADR
[0010](adr/0010-clean-rebuild-from-prototype.md)), and wrote down what the real thing
should be.

## Research before code

The first idea was "a read-only Discord MCP server". Searching GitHub showed that space was
already full of plain API wrappers. A wrapper hands an agent raw messages; it does not say
what the team *decided*. That settled the pitch:

> Keep a **cited record** of decisions, tasks and open questions that people and agents can
> ask, and make it work without an LLM.

Design choices that followed:

- **Self-hosted, one program per team.** Chat stays on a machine the team controls, and
  message content is never used to train anything.
- **People decide what goes in the record.** A wrong auto-extracted "decision" is worse
  than a missing one.
- **Chat is untrusted input.** Every output that carries chat text is fenced as data, so an
  agent reading it is told not to follow instructions inside it.
- **Read-only towards Discord.** Two permissions, and a test that fails if the code ever
  calls a write endpoint.

The plan has 13 assumed decisions, 14 architecture decision records (ADRs) and a set of
milestones, each with "done when" criteria.

## How the build ran

Work was split into tasks with clear ownership, run in parallel by agents, and reviewed by
an independent second model after each batch. A change merged only after the named checks
had run: formatting, lints as errors, the full test suite, a dependency audit, a minimum
Rust version check and the retrieval eval.

| Date (2026) | What happened |
|---|---|
| Nov 2 | Private prototype. Lessons collected. |
| Nov 4 | Research, plan, ADRs, CI on three operating systems, security workflows. |
| Nov 5 | MCP over HTTP and SQLite full-text search spikes. Core types, store, fictional demo team, retrieval eval, tokens, Discord event mapping, MCP tools, sync against a fake Discord. |
| Nov 10 | The team host (`teamctx run`), slash commands and context menus, export, JSON API, CLI client, Agent Skill and `/ask`. |

## What I learned

1. **Search before you build.** My first idea was taken, and finding out on day one was cheap.
2. **Write "done when" before the code.** It is what lets you say no to "finished".
3. **Make the eval able to fail.** My first retrieval baseline was all zeros, so it could not
   catch a regression. Any-word matching took recall@10 from 0.00 to 0.61, and making the
   eval run the real code path brought the figure to 0.54.
4. **Audit agent output like a stranger's pull request.** My audit replaced a templated demo
   dataset with 314 realistic messages and tightened the store code. Independent review
   caught real concurrency and input-bounding bugs, each fixed with a regression test.
5. **Read the checks, not the summary.** A passing workflow is not the same as every check
   passing.
6. **Treat chat as hostile input**, even from your own team and in your own tests.

## Testing without a live server

| Layer | What it is | What it covers |
|---|---|---|
| **Fictional team** | "Lantern Shelf": 6 people and a bot, 4 channels, 6 threads, 314 messages, 76 record entries, 2 planted prompt-injection messages | Search, ranking and the record on realistic, messy chat |
| **Retrieval eval** | 62 questions in five types (date, name, reversed decision, paraphrase, thread context), scored by recall@10 and MRR, gated in CI | Retrieval quality is measured and cannot silently drop |
| **Recorded Discord payloads** | Gateway events, REST responses and interactions as fixtures, plus a fake REST client | Event mapping, catch-up, edits and deletes |
| **A fake model** | A scripted provider instead of a real LLM | The `/ask` pipeline, citation checks and budgets |
| **Hostile-message corpus** | 30+ attacks across ten classes, including fake delimiters, bidi and zero-width characters, markdown links and fake citations | Chat text stays fenced through MCP, the API and `/ask` |
| **Clean machines** | A throwaway Linux VM running the CI checks, plus a Windows Sandbox setup | It builds and passes outside my own machine |
| **Local Discord emulator** *(planned, M3b)* | The real `teamctx run` binary against an open-source emulator with six scripted users | The whole binary end to end, repeatably |
| **Real test server** *(next)* | A throwaway server and bot | Live gateway and interaction behaviour |

## By the numbers

![Passing tests at five checkpoints, from 99 to 678](assets/tests-growth.svg)

| What | Value |
|---|---|
| Automated tests passing | 678 |
| Operating systems in CI | Linux, macOS, Windows |
| Architecture decision records | 14 |
| Demo team | 314 messages, 76 entries, 62 eval questions |
| Retrieval recall@10 | 0.54 for search, 0.65 for the `/ask` bundle |
| Checks before a merge | formatting, lints as errors, tests, dependency audit, minimum Rust version, eval gate, CodeQL, workflow lint |

Measured on 10 November 2026. Reproduce with `cargo test --workspace` and
`cargo xtask eval -- --check`.

## Status

Pre-alpha. Everything above runs against the fictional demo team and test fixtures; live
Discord and model testing is the next milestone. See the [roadmap](roadmap.md).
