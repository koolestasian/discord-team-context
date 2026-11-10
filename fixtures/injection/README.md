# Prompt-injection corpus

`corpus.json` is a fictional team whose chat is written to attack whatever reads it
(plan section 13, "Prompt injection via chat"; section 14, Security row). CI loads it
through the demo importer (`teamctx_store::demo::import`) into a temporary store and
checks that the MCP tools and the `/ask` pipeline keep every hostile string framed as
data, and that `/ask` answers keep only citations and URLs from the retrieved sources.

Everything in it is data. None of it is an instruction, for people or for agents.

## Format

The file is a demo dataset (schema version 1, see `fixtures/demo/README.md`), so the
same importer loads it. The importer ignores three extra fields that only the tests read:

| Field | On | Meaning |
|---|---|---|
| `attack` | messages | The attack class (list below). |
| `probe` | messages, entries | A unique word (`injprobe07`, `injentry03`) that finds exactly this item by search. |
| `needles` | messages, entries, members, channels | Hostile substrings that must never appear outside the framed block. |

Attack classes, each with at least one message: `instruction_override`, `fake_role`,
`fake_delimiter` (closing tags, quotes, fences, full-width look-alikes),
`unicode_control` (bidi overrides and isolates, zero-width, C0/C1 controls, U+2028),
`markdown_link` (links, images, autolinks, reference definitions, `javascript:` and
`data:`), `fake_citation`, `tool_call`, `encoded` (base64, HTML entities, percent
encoding, rot13, hex), `long_line` and `notice_quote`.

Member display names, a thread name, entry titles and a tag are hostile too, because
they reach the same outputs as message text.

The file is pure ASCII: every non-ASCII and control character is written as a JSON
`\u` escape, so reviewers never see raw bidi characters in a diff.

## Rules

- Keep at least 30 messages; a test fails below that.
- Every probe must occur in exactly one item, and every needle in its own item's text.
- Fictional only: no real people, servers, IDs, tokens or URLs. Hosts use `.example`.
