# Roadmap

What is left before `v0.1.0`, what comes after, and the ideas that are worth keeping on a
list but not on the critical path. The [implementation plan](design/implementation-plan.md)
is the spec for each milestone; this page is the short version and the backlog.

Status on 10 November 2026: milestones M0 to M6 are built and tested against fakes and a
fictional demo team. Nothing has run against a real Discord server yet.

## To v0.1.0 and the launch

| M | Goal | Done when |
|---|---|---|
| **M1 live** | Settle spikes S1 (gateway delivers message content) and S2 (interaction replies work without Send Messages) on a throwaway server | Both have a written result in the ADRs. If they are not working within four working days of effort, reconsider Go (plan section 19) |
| **M3b** | **End-to-end simulation.** The real `teamctx run` binary against a local Discord emulator: six scripted users, a host outage and catch-up, gateway reconnect, captures through real commands, a scored report | The scenario runs from one command, every check passes or is marked not applicable with a reason, and the report is readable by a human. Needs the loopback API address (ADR 0016) |
| **M3 to M6 live** | Run live sync, the commands and `/ask` against the test server and one hosted and one local model | The live checks in the plan's user stories pass, with evidence |
| **M7** | **Simple web view.** The three screens in plan section 10.2: Record (decisions, tasks, questions), Ask and search, Status | Plain HTML and CSS served by `teamctx`, no JavaScript framework, works at 360 px wide, keyboard friendly, behind the same login. A Figma sketch is optional. Nothing fancy on purpose |
| **M8** | Harden and release `v0.1.0` | Threat model and privacy guide, hosting guides, one-line installer, container image, clean install on Windows, macOS and Linux, tag `v0.1.0` |
| **M9** | **Show it.** Final README with screenshots and a short GIF, social preview image, **a 45 to 75 second promo video**, and **a 3 to 5 minute end-to-end video** | The end-to-end video goes from a fresh install to a real question answered from a real Discord server, with captions. Both are linked from the README. Nothing in either video is staged: every output shown comes from a real run |
| **M10** | **Launch.** LinkedIn, X, Reddit and Hacker News posts, then one week of replying and fixing | Each post follows that community's rules (including saying the project was built with AI agents). Issues from the first week are triaged and `v0.1.1` ships the quick fixes |

M9 and M10 come after M8 on purpose. A video of something that is not released shows
people something they cannot try, and a launch is easiest to do once.

## After v0.1 (from the plan)

- **v0.2:** Slack connector (each team's own internal app), GitHub connector (issues and
  pull requests as context), model-suggested entries with a human review queue, OAuth 2.1
  for MCP, more than one server per host.
- **v0.3:** import notes from Markdown or exported documents, optional embeddings
  (`sqlite-vec`), a session-start hook for agents.

## Goals worth adding

These are ideas, ranked by value for the effort. None of them is on the critical path to
`v0.1.0`. New ideas go here, not into a milestone (plan section 19, "scope creep").

### Quick wins (a day or less each)

- **A GIF that cannot lie.** Generate the README demo from a script
  ([VHS](https://github.com/charmbracelet/vhs) tape files) committed to the repository, so
  anyone can regenerate it and it can never drift from what the program prints.
- **Fuzz the query parser.** Search text is attacker-controlled input. A `cargo-fuzz`
  target for the parser addresses the open "fuzzing" finding from the OpenSSF Scorecard and is a
  concrete, measurable security claim.
- **Reserve the name.** `teamctx` is a working name. At the time of writing the crate name
  appeared unclaimed on crates.io; decide the final name before M8 and claim it.
- **Good first issues.** A handful of small, well-described issues (a new export format,
  a timezone name lookup, a doctor check) so a visitor can contribute in an hour.

### Portfolio-grade (a few days each, strong on a resume)

- **Agent A/B demo.** Give an AI coding agent the same small task with and without
  teamctx, where the correct answer depends on a *reversed* decision ("the beta date moved
  twice"). Record both runs. This is the clearest possible proof of value and the best
  content for a launch post.
- **Publish the benchmark.** The fictional team, its 62 questions and the hostile-message
  corpus are already a small benchmark for "team memory": reversed decisions, paraphrase,
  thread context and prompt injection. Package it so other tools can be scored against it.
- **Beat paraphrase search.** Paraphrase recall is 0.27 today. Add embeddings
  (`sqlite-vec`) behind a setting, measure against the same eval, and publish the
  before and after, including if it does not help enough to justify the dependency.
- **A prompt-injection write-up.** What the 30-plus attacks were, what held, what nearly
  did not. Short, concrete and useful to anyone building an agent that reads chat.
- **Signed releases with provenance.** Release binaries with build attestations so a
  stranger can verify what they downloaded.

### Product stretch

- **Time travel.** `teamctx decisions --as-of 2026-03-20`: what did the team believe on
  that date? The record already keeps the chain of reversals, so this is cheap, and I have
  not seen another tool offer it.
- **Decision graph.** In the web view, draw the chain of a decision that was reversed
  (the beta date, reversed twice in the demo). It is also the best single screenshot for the README.
- **Git sync.** Export the record into a Git repository on a schedule, so decisions live
  next to the code and a pull request can say "per D-7".
- **Drift alerts.** Flag a task whose blocking decision was later reversed, or a decision
  nobody has touched in a long time. Read-only: it shows up in `/catchup` and the API, it
  never posts.
- **Session-start hook.** When an agent starts work, it is told what changed in the record
  since it last ran, without being asked.
- **Feeds and digests.** An RSS or Atom feed of new decisions, and a weekly digest written
  to a file, never posted into Discord.
- **More chat apps.** Slack and Matrix first. The core was kept free of I/O so a second
  connector does not mean a rewrite.

### Wild ideas (probably not, but fun to think about)

- **Try it in the browser.** A static page with the demo team and a precomputed index, so
  nobody has to install anything to see a search. Heavy to build; a recorded GIF gets 80% of
  the value.
- **A "student team" kit.** A one-page guide and a template for class projects, case
  competitions and hackathons, which is where teams live in Discord and decide things fast.
- **Upstream something.** Write up the loopback-emulator approach for testing Discord bots
  and share it with the emulator's authors, or contribute a fix to a dependency you used.
  Contributions to other people's projects are the best resume line there is.

## What this project will not do

These are decisions, not gaps:

- **Write to Discord.** No posting, reacting, editing, deleting or moderating. Replies to
  commands only.
- **Guess the record.** Entries are captured by people. A model may suggest in a future
  version, but never adds silently.
- **Become a hosted service.** Self-hosted, one program per team. No accounts, no cloud.
- **Train on or resell chat.** Message content is never used to train a model.

## Suggested order

1. Live spikes S1 and S2, then M3 to M6 live (needs the test server).
2. M3b in parallel, since it needs only a local emulator.
3. M7, then M8 and the release.
4. Quick wins while waiting on anything: the VHS GIF, the fuzz target, the name.
5. M9 videos, then M10 launch. Pick one portfolio-grade goal (the agent A/B demo is the
   strongest) and finish it *before* launching, so the first post has a number in it.
