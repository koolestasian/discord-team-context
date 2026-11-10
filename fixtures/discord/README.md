# Discord payload fixtures

Recorded-shape JSON for the offline mapping tests in
`crates/teamctx-discord/src/mapping/`. Everything here is fictional: ids start with
`11000000000000`, names are made up, and URLs use the reserved `example.invalid` domain.
Nothing was captured from a real server.

- `gateway/`: one gateway frame (`{"op", "s", "t", "d"}`) per file, named after the event
  and the variation it covers (`message_create_bot`, `thread_create_forum_post`, ...).
- `rest/`: REST response bodies used for catch-up and discovery.
- `interactions/`: a `READY` frame and `INTERACTION_CREATE` frames (a message context
  menu, `/catchup`, `/ask`, a public `/decide` with mention and markdown text) for the interaction
  tests. Their tokens start with `FAKE-` and are not credentials.
  `interaction_fixtures_parse_and_hold_only_fictional_ids` lists the files.

`message_create_injection.json` holds message text that tries to give instructions. The
mapping must carry it as plain data, and a test checks that it does.

Adding a file here requires adding it to the coverage tables in
`crates/teamctx-discord/src/mapping/tests.rs`; `every_fixture_file_is_covered` fails
otherwise.
