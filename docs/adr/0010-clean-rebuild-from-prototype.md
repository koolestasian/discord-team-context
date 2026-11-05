# 0010. Clean rebuild from the prototype

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D10, §11, §7.1, §13

## Context

Before this project, the maintainer built a private prototype of a Discord-to-agent
tool. It taught useful lessons about what to build and what to avoid. The plan
mentions two of them: the permission checker, whose channel-overwrite handling needs
to be done correctly this time (§7.1), and an agentic loop for answering questions,
which `/ask` deliberately replaces with a single call (§11).

This repository is public and must be credible: no real team data, names, IDs, tokens
or configuration may appear in it, including in tests, fixtures and docs (§13 and the
repository rules).

## Decision

`teamctx` is a clean rebuild. The prototype is a reference for ideas only. It is
read for lessons and never copied from:

- no code is carried over by copy-paste or by porting line by line;
- no data, message text, member names, IDs, tokens, channel or server identifiers, or
  configuration from the prototype enter this repository in any form;
- the demo dataset and all fixtures are fictional and written for this project;
- recorded Discord payloads in `fixtures/discord/` come from a dedicated test server
  or are synthesised, never from a team's server.

Lessons from the prototype are written down as design decisions in the plan and in
ADRs, in terms that do not reveal who it was built for.

## Alternatives considered

- **Fork or continue the prototype.** Faster start for features that already exist,
  but it would inherit its structure and design choices (including the agentic loop
  that `/ask` replaces), and any private identifiers or data in its files or history
  would need to be found and removed.
- **Port the prototype module by module.** A middle path, but it still carries over
  decisions made for a different design and invites accidental carry-over of fixtures
  or constants.

## Consequences

### Good

- The public history starts clean and every file has a known origin.
- The design is shaped by the plan's boundaries (ADR 0005, ADR 0011), not by what the
  prototype happened to do.

### Bad

- Rebuilding costs time that porting might have saved, and some bugs the prototype
  already fixed may be rediscovered.
- Lessons must be recorded deliberately, or they are lost.

### Follow-ups

- Each milestone's prior-art note (`docs/research/M<N>.md`) records what was learned
  from outside sources.
- Reviewers check new fixtures for real identifiers before merge. Secret scanning and
  push protection are enabled (§16).
