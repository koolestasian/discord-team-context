# 0013. Open Knowledge Format export

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D12, §10.3, §10.5, §14

## Context

The record (decisions, tasks and open questions, each citing its source message) is
the most valuable data the program makes. A team should be able to read it, keep it
and move it without our software: for a school project handed in, committed to the
team's repository so agents can grep it, backed up, or moved to another tool (plan
§10.5).

The plan points to the Open Knowledge Format (OKF) 0.2, from Google Cloud (June 2026),
which builds on Karpathy's April 2026 "LLM wiki" pattern of cross-linked Markdown.

## Decision

`teamctx export --format okf <dir>` writes the record as an OKF 0.2 bundle:

- `index.md` with frontmatter `okf_version: "0.2"` and a list of all entries;
- `log.md`, the change history from `record_events`;
- one file per entry under `decisions/`, `tasks/` and `questions/`, named like
  `D-12-use-sqlite.md`.

Each entry has frontmatter `type: decision|task|question`, status, owner, dates,
`supersedes` and `superseded_by` links, and `sources` (`id`, `resource` as the Discord
jump link, `title`, `author`). Claims cite sources as footnotes keyed by source `id`.
Human-captured entries use the spec's `human:` actor prefix; since every v0.1 entry is
human-captured, this marks them as human-confirmed.

The command also accepts `--format md` (§10.3); the plan does not specify that layout,
so it is defined in M5. A live mirror to a folder or git repository is a v0.2 item.

Bundle structure and frontmatter are covered by snapshot tests on the demo record
(§14).

## Alternatives considered

- **JSON dump only.** Easy to produce and restore, but not pleasant to read or grep,
  and not self-describing for people outside the project.
- **Copy of the SQLite file.** Complete, but needs SQLite tooling and our schema to
  interpret, and would carry raw message text.
- **Our own Markdown layout.** Full freedom, but every consumer would need to learn
  it; a published format gives a shared target.
- **Export nothing in v0.1.** Smaller scope, but the portability promise would be
  untested, and the record would be locked into one tool.

## Consequences

### Good

- Agents and people can read the record with no teamctx software installed.
- Source footnotes keep the link back to the original message.

### Bad

- OKF is young (0.2). We depend on a third-party spec we do not control and may have
  to track revisions.
- Exports can contain titles copied from chat. The deletion rule (§6.1) applies to
  new exports, but a bundle already written stays as it was.

### Follow-ups

- M5: implement `export --format okf` with snapshot tests.
- Record the spec version in `index.md` so consumers can detect changes.

## Implementation notes (M5, 2026-11-09)

Details settled while building the export; see `docs/research/M5.md` for sources.

- **`--format md` layout.** One file, `record.md`: `# Team record`, `## Decisions`,
  `## Tasks`, `## Questions` (one `###` section per entry with a bullet list of its
  facts), and `## History` (every audit event, oldest first). No frontmatter, footnotes
  or directories, so it reads the same in any viewer. It is for people; the OKF bundle is
  for agents and repositories. It holds the same facts as the bundle.
- **`status` in OKF frontmatter.** OKF 0.2 limits `status` to `draft`, `stable` and
  `deprecated`. The entry's `status` carries that value (open is `draft`; active, done and
  answered are `stable`; superseded and dropped are `deprecated`) and `entry_status`
  carries the exact lifecycle status. This departs from the plan's wording ("status") and
  needs the maintainer's confirmation.
- **Actors.** `generated.by` and `verified.by` are `human:discord-<user id>` of the person
  who captured the entry; `verified` is what the spec's trust tiers read as human-reviewed.
- **No message text.** Only titles (which may be copied from chat), names, ids, dates and
  jump links are written. Titles and names are escaped for file names, YAML and Markdown,
  and each file says they are data.
- **Writing.** `export` refuses a directory that has anything in it. `--force` replaces
  only files an export wrote (matched by exact name pattern), never follows a symbolic
  link, and creates files with `create_new`. Times are UTC.
