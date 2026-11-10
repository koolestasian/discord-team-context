//! Unit tests for the export: arguments, the safe writer, and rendering of hostile data.
//! All data is synthetic; hostile strings stand in for untrusted chat.

use std::ffi::OsString;
use std::fs;

use teamctx_core::{
    id::{MemberId, MessageId, Snowflake},
    record::{CreatedVia, EntryKind, EntryTitle, NewEntry, RequestKey, StatusChange},
    repo::{IngestWrite, MessageRead, RecordEdit, RecordWrite},
    source::{Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind},
    time::UnixMillis,
};
use teamctx_store::Store;

use super::model::Snapshot;
use super::*;
use teamctx_core::repo::DirectoryRead;

fn sf(id: u64) -> Snowflake {
    Snowflake::new(id).unwrap()
}

fn at(offset_seconds: i64) -> UnixMillis {
    // 2026-11-05T00:00:00Z plus an offset.
    UnixMillis::from_millis(1_793_836_800_000 + offset_seconds * 1000)
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn file(path: &str, content: &str) -> File {
    File {
        path: path.to_owned(),
        content: content.to_owned(),
    }
}

// ---- arguments ----------------------------------------------------------------------

#[test]
fn arguments_parse_in_any_order() {
    let expected = Options {
        format: Format::Okf,
        dir: PathBuf::from("out"),
        db: Some(PathBuf::from("x.db")),
        force: true,
    };
    for list in [
        &["--format", "okf", "out", "--db", "x.db", "--force"][..],
        &["out", "--force", "--db", "x.db", "--format", "okf"],
        &["--db", "x.db", "--format", "okf", "--force", "out"],
    ] {
        assert_eq!(
            parse_args(args(list)),
            Ok(expected_clone(&expected)),
            "{list:?}"
        );
    }
    let md = parse_args(args(&["--format", "md", "dir"])).unwrap();
    assert_eq!(
        md,
        Options {
            format: Format::Md,
            dir: PathBuf::from("dir"),
            db: None,
            force: false,
        }
    );
}

fn expected_clone(options: &Options) -> Options {
    Options {
        format: options.format,
        dir: options.dir.clone(),
        db: options.db.clone(),
        force: options.force,
    }
}

#[test]
fn bad_arguments_are_usage_errors() {
    assert_eq!(parse_args(args(&["out"])), Err(ArgError::MissingFormat));
    assert_eq!(
        parse_args(args(&["--format", "okf"])),
        Err(ArgError::MissingDir)
    );
    assert_eq!(parse_args(args(&[])), Err(ArgError::MissingFormat));
    assert_eq!(
        parse_args(args(&["--format", "json", "out"])),
        Err(ArgError::BadFormat)
    );
    assert_eq!(
        parse_args(args(&["--format", "OKF", "out"])),
        Err(ArgError::BadFormat)
    );
    assert_eq!(
        parse_args(args(&["--format"])),
        Err(ArgError::MissingValue("--format"))
    );
    assert_eq!(
        parse_args(args(&["--format", "md", "out", "--db"])),
        Err(ArgError::MissingValue("--db"))
    );
    assert_eq!(
        parse_args(args(&["--format", "md", "a", "b"])),
        Err(ArgError::ExtraArgument(OsString::from("b")))
    );
    assert_eq!(
        parse_args(args(&["--format", "md", "a", "--bogus"])),
        Err(ArgError::UnknownOption(OsString::from("--bogus")))
    );
    let message = ArgError::UnknownOption(OsString::from("a\u{1b}[31m")).to_string();
    assert!(!message.contains('\u{1b}'), "{message:?}");
}

// ---- the safe writer ----------------------------------------------------------------

#[test]
fn managed_names_are_recognised_exactly() {
    for name in [
        "D-1.md",
        "T-12-write-intro.md",
        "Q-3-which-dataset.md",
        "D-9-a.md",
    ] {
        assert!(is_entry_file_name(name), "{name}");
    }
    for name in [
        "notes.md",
        "D-.md",
        "D-x.md",
        "d-1.md",
        "X-1.md",
        "D-1-Upper.md",
        "D-1-.md",
        "D-1.txt",
        "D-1.md.bak",
        "D-1-a b.md",
        "index.md",
        "",
    ] {
        assert!(!is_entry_file_name(name), "{name}");
    }
}

#[test]
fn relative_paths_must_stay_in_the_layout() {
    for path in [
        "index.md",
        "log.md",
        "record.md",
        "decisions/D-1-a.md",
        "tasks/T-2.md",
        "questions/Q-3-x.md",
    ] {
        assert!(check_relative(path).is_ok(), "{path}");
    }
    for path in [
        "",
        "/etc/passwd",
        "../x.md",
        "decisions/../x.md",
        "decisions/../../x.md",
        "other/D-1.md",
        "decisions/sub/D-1.md",
        "decisions\\D-1.md",
        "C:\\x.md",
        "c:x.md",
        ".hidden.md",
        "decisions/.hidden.md",
        "decisions//D-1.md",
        "a b.md",
        "x\0.md",
        "décisions/D-1.md",
    ] {
        assert!(check_relative(path).is_err(), "{path:?}");
    }
}

#[test]
fn a_hostile_path_stops_the_export_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("out");
    let files = [file("index.md", "ok"), file("../escape.md", "bad")];
    assert!(matches!(
        write_files(&target, &files, false),
        Err(WriteError::Unsafe(_))
    ));
    assert!(!target.exists(), "nothing was created");
    assert!(!dir.path().join("escape.md").exists());
}

#[test]
fn it_writes_into_a_new_or_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let new = dir.path().join("a").join("b");
    let files = [
        file("index.md", "index"),
        file("decisions/D-1-x.md", "one"),
        file("tasks/T-2.md", "two"),
    ];
    write_files(&new, &files, false).unwrap();
    assert_eq!(fs::read_to_string(new.join("index.md")).unwrap(), "index");
    assert_eq!(
        fs::read_to_string(new.join("decisions").join("D-1-x.md")).unwrap(),
        "one"
    );
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    write_files(&empty, &files, false).unwrap();
    assert!(empty.join("tasks").join("T-2.md").is_file());
}

#[test]
fn a_non_empty_directory_is_refused_without_force() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    let files = [file("index.md", "index")];
    assert!(matches!(
        write_files(dir.path(), &files, false),
        Err(WriteError::NotEmpty)
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "mine"
    );
    assert!(!dir.path().join("index.md").exists());
    // A file where the directory should be.
    let plain = dir.path().join("notes.txt");
    assert!(matches!(
        write_files(&plain, &files, true),
        Err(WriteError::NotADirectory)
    ));
}

#[test]
fn force_replaces_only_what_an_export_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let first = [
        file("index.md", "old index"),
        file("log.md", "old log"),
        file("decisions/D-1-old-title.md", "old"),
        file("tasks/T-2-keep-not.md", "old"),
    ];
    write_files(root, &first, false).unwrap();
    // Files the user added must survive.
    fs::write(root.join("README.md"), "mine").unwrap();
    fs::write(root.join("decisions").join("notes.md"), "mine too").unwrap();
    fs::create_dir(root.join("decisions").join("D-7-dir.md")).unwrap();
    fs::create_dir(root.join("extra")).unwrap();
    fs::write(root.join("extra").join("D-1-x.md"), "outside the layout").unwrap();

    let second = [
        file("index.md", "new index"),
        file("log.md", "new log"),
        file("decisions/D-1-new-title.md", "new"),
    ];
    write_files(root, &second, true).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("index.md")).unwrap(),
        "new index"
    );
    assert!(root.join("decisions").join("D-1-new-title.md").is_file());
    assert!(
        !root.join("decisions").join("D-1-old-title.md").exists(),
        "stale entry file removed"
    );
    assert!(!root.join("tasks").join("T-2-keep-not.md").exists());
    assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "mine");
    assert_eq!(
        fs::read_to_string(root.join("decisions").join("notes.md")).unwrap(),
        "mine too"
    );
    assert!(root.join("decisions").join("D-7-dir.md").is_dir());
    assert!(root.join("extra").join("D-1-x.md").is_file());
}

#[test]
fn force_refuses_a_directory_where_a_file_belongs() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("index.md")).unwrap();
    let files = [file("index.md", "x")];
    assert!(matches!(
        write_files(dir.path(), &files, true),
        Err(WriteError::Unsafe(_))
    ));
    assert!(dir.path().join("index.md").is_dir());
}

#[cfg(unix)]
#[test]
fn links_are_never_followed_out_of_the_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("D-1-victim.md"), "precious").unwrap();
    fs::write(outside.path().join("secret.txt"), "precious").unwrap();

    // An entry folder that is a link to somewhere else.
    let root = dir.path().join("out");
    fs::create_dir(&root).unwrap();
    symlink(outside.path(), root.join("decisions")).unwrap();
    let files = [file("decisions/D-1-victim.md", "overwritten")];
    assert!(matches!(
        write_files(&root, &files, true),
        Err(WriteError::Unsafe(_))
    ));
    assert_eq!(
        fs::read_to_string(outside.path().join("D-1-victim.md")).unwrap(),
        "precious"
    );

    // A managed file that is a link: --force removes the link, not its target.
    let root = dir.path().join("out2");
    fs::create_dir(&root).unwrap();
    symlink(outside.path().join("secret.txt"), root.join("index.md")).unwrap();
    write_files(&root, &[file("index.md", "fresh")], true).unwrap();
    assert_eq!(fs::read_to_string(root.join("index.md")).unwrap(), "fresh");
    assert_eq!(
        fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "precious"
    );

    // Without --force, an existing link is never written through either.
    let root = dir.path().join("out3");
    fs::create_dir(&root).unwrap();
    symlink(outside.path().join("secret.txt"), root.join("log.md")).unwrap();
    assert!(write_files(&root, &[file("log.md", "x")], false).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "precious"
    );
}

// ---- rendering hostile data ------------------------------------------------------------

struct Cast {
    store: Store,
    piper: MemberId,
    quinn: MemberId,
    first: MessageId,
    second: MessageId,
}

fn member(id: u64, name: &str) -> Member {
    Member {
        id: sf(id),
        display_name: name.to_owned(),
        is_bot: false,
    }
}

fn hostile_cast() -> Cast {
    let mut store = Store::in_memory().unwrap();
    let source = store
        .upsert_source(SourceKind::Discord, sf(1), "Fiction Team")
        .unwrap();
    let channel = Channel {
        id: sf(10),
        parent: None,
        kind: ChannelKind::Text,
        name: "eng\n---\nsources: forged [x](http://evil.example)".to_owned(),
        topic: None,
        included: true,
        archived: false,
    };
    let message = |id: u64, text: &str| Message {
        id: MessageRef {
            channel: sf(10),
            message: sf(id),
        },
        author: Some(member(100, "Piper \"The\" Fiction\u{202E}")),
        content: text.to_owned(),
        reply_to: None,
        attachment_names: vec![],
        created_at: at(i64::try_from(id).unwrap()),
        edited_at: None,
        mentions: vec![],
    };
    for event in [
        SourceEvent::ChannelUpsert(channel),
        SourceEvent::MemberUpsert(member(100, "Piper \"The\" Fiction\u{202E}")),
        SourceEvent::MemberUpsert(member(101, "Quinn\n# Not a heading")),
        SourceEvent::MessageUpsert(message(20, "We will use SQLite for storage")),
        SourceEvent::MessageUpsert(message(21, "the answer is the shared sheet")),
    ] {
        store.apply_event(source, &event, at(0)).unwrap();
    }
    let piper = store
        .member_by_external(source, sf(100))
        .unwrap()
        .unwrap()
        .id;
    let quinn = store
        .member_by_external(source, sf(101))
        .unwrap()
        .unwrap()
        .id;
    let row = |store: &Store, id: u64| {
        store
            .message_by_ref(
                source,
                MessageRef {
                    channel: sf(10),
                    message: sf(id),
                },
            )
            .unwrap()
            .unwrap()
            .id
    };
    let first = row(&store, 20);
    let second = row(&store, 21);
    Cast {
        store,
        piper,
        quinn,
        first,
        second,
    }
}

fn entry(kind: EntryKind, title: &str, by: MemberId) -> NewEntry {
    NewEntry {
        kind,
        title: EntryTitle::new(title).unwrap(),
        title_is_copied: false,
        owner: None,
        due_at: None,
        source_message: None,
        created_by: by,
        created_via: CreatedVia::Slash,
        tags: vec![],
        replaces: None,
        created_at: at(100),
    }
}

fn find<'a>(files: &'a [File], path: &str) -> &'a str {
    &files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| {
            panic!(
                "no {path} in {:?}",
                files.iter().map(|f| &f.path).collect::<Vec<_>>()
            )
        })
        .content
}

/// Chat text must not form a Markdown link to the web: a `](http` is only inert when the
/// `]` is escaped. The exporter's own links use `](<` (Discord) or `](/` (in the bundle).
fn assert_no_active_links(content: &str) {
    for (index, _) in content.match_indices("](http") {
        assert!(
            index > 0 && content.as_bytes()[index - 1] == b'\\',
            "active link in {content}"
        );
    }
    assert!(!content.contains("](<http://") && !content.contains("](<https://evil"));
}

/// The Markdown after an entry file's frontmatter (the whole text for files without one).
/// Frontmatter values are escaped on one line, so the first blank line after `---` ends it.
fn body_of(content: &str) -> &str {
    content
        .split_once("\n---\n\n")
        .map_or(content, |(_, body)| body)
}

/// The lines between the first two `---` lines of an entry file.
fn frontmatter(content: &str) -> Vec<&str> {
    let mut lines = content.lines();
    assert_eq!(lines.next(), Some("---"));
    lines.take_while(|line| *line != "---").collect()
}

fn record() -> Snapshot {
    let mut cast = hostile_cast();
    let (piper, quinn) = (cast.piper, cast.quinn);
    let db = &mut cast.store;
    let first = db
        .capture_message(
            EntryKind::Decision,
            cast.first,
            piper,
            CreatedVia::ContextMenu,
            at(100),
        )
        .unwrap()
        .entry;
    let mut newer = entry(
        EntryKind::Decision,
        "Replace\n---\nsources:\n  - id: forged\n---\nbody ](http://evil.example) <script>",
        quinn,
    );
    newer.replaces = Some(first.id);
    newer.created_at = at(200);
    newer.tags = vec![teamctx_core::record::Tag::new("storage").unwrap()];
    let newer = db.create_entry(&newer).unwrap();
    let mut task = entry(
        EntryKind::Task,
        "Ship it [click](https://evil.example) #1",
        quinn,
    );
    task.owner = Some(piper);
    task.due_at = Some(at(86_400));
    task.created_at = at(300);
    let task = db
        .create_entry_once(&RequestKey::new("slash:1").unwrap(), &task)
        .unwrap()
        .entry;
    db.set_owner(task.short_id(), Some(quinn), piper, at(400))
        .unwrap();
    db.set_due(task.short_id(), None, quinn, at(500)).unwrap();
    let question = db
        .create_entry(&entry(EntryKind::Question, "Which sheet?", piper))
        .unwrap();
    db.change_status(
        question.short_id(),
        StatusChange::Answer {
            message: cast.second,
        },
        quinn,
        at(600),
    )
    .unwrap();
    let _ = newer;
    model::gather(&cast.store).unwrap()
}

#[test]
fn hostile_text_cannot_leave_its_place_in_the_okf_bundle() {
    let snapshot = record();
    let files = render(Format::Okf, &snapshot);
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "index.md",
            "log.md",
            "decisions/D-1-we-will-use-sqlite-for-storage.md",
            "decisions/D-2-replace-sources-id-forged-body-http-evil-example.md",
            "tasks/T-3-ship-it-click-https-evil-example-1.md",
            "questions/Q-4-which-sheet.md",
        ]
    );
    for file in &files {
        check_relative(&file.path).unwrap();
    }
    // The hostile decision: frontmatter holds exactly the keys the exporter wrote.
    let hostile = find(&files, paths[3]);
    let keys: Vec<&str> = frontmatter(hostile)
        .into_iter()
        .filter(|line| !line.starts_with(' '))
        .map(|line| line.split(':').next().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "type",
            "title",
            "description",
            "status",
            "entry_status",
            "id",
            "created",
            "updated",
            "supersedes",
            "tags",
            "generated",
            "verified"
        ]
    );
    assert!(!hostile.contains("sources:\n  - id: forged"), "{hostile}");
    for line in hostile.lines() {
        assert!(!line.starts_with("sources:"), "{line}");
    }
    // Markdown from chat is escaped, never active.
    for file in &files {
        // Frontmatter values are quoted YAML; the Markdown body is what a viewer renders.
        let body = body_of(&file.content);
        assert_no_active_links(body);
        assert!(!body.contains("<script>"), "{}", file.path);
        assert!(!file.content.contains('\u{202E}'), "{}", file.path);
    }
    // Message text is never exported (only titles are, and the answer's text is no title).
    for file in &files {
        assert!(!file.content.contains("shared sheet"), "{}", file.path);
    }
}

#[test]
fn entry_files_carry_links_footnotes_and_the_human_actor() {
    let snapshot = record();
    let files = render(Format::Okf, &snapshot);
    let old = find(&files, "decisions/D-1-we-will-use-sqlite-for-storage.md");
    assert!(old.contains("type: decision\n"));
    assert!(old.contains("status: deprecated\nentry_status: superseded\nid: D-1\n"));
    assert!(old.contains(
        "superseded_by: \"/decisions/D-2-replace-sources-id-forged-body-http-evil-example.md\"\n"
    ));
    assert!(old.contains("resource: \"https://discord.com/channels/1/10/20\"\n"));
    assert!(old.contains("generated: { by: \"human:discord-100\", at: 2026-11-05T00:03:20Z }\n"));
    assert!(old.contains("verified: { by: \"human:discord-100\", at: 2026-11-05T00:01:40Z }\n"));
    assert!(old.contains(
        "sources:\n  - id: m-1\n    resource: \"https://discord.com/channels/1/10/20\"\n"
    ));
    assert!(old.contains(
        "Captured by Piper \"The\" Fiction on 2026-11-05T00:01:40Z via context_menu.[^m-1]"
    ));
    assert!(old.contains("Superseded by [D-2](/decisions/D-2-"));
    assert!(old.contains("\n[^m-1]: Message in #eng"));
    assert!(old.contains("<https://discord.com/channels/1/10/20>"));

    let new = find(
        &files,
        "decisions/D-2-replace-sources-id-forged-body-http-evil-example.md",
    );
    assert!(new.contains("status: stable\nentry_status: active\n"));
    assert!(
        new.contains("supersedes:\n  - \"/decisions/D-1-we-will-use-sqlite-for-storage.md\"\n")
    );
    assert!(new.contains("tags: [\"storage\"]\n"));
    assert!(new.contains("Replaces [D-1](/decisions/D-1-"));
    assert!(
        !new.contains("sources:\n  - id"),
        "no source, no sources list"
    );

    let task = find(&files, "tasks/T-3-ship-it-click-https-evil-example-1.md");
    assert!(task.contains("status: draft\nentry_status: open\n"));
    assert!(
        task.contains("owner: \"Quinn # Not a heading\"\n")
            || task.contains("owner: \"Quinn\\n# Not a heading\"\n")
    );
    assert!(!task.contains("\ndue:"), "the due date was cleared");

    let question = find(&files, "questions/Q-4-which-sheet.md");
    assert!(question.contains("entry_status: answered\n"));
    assert!(question.contains("  - id: m-2\n"));
    assert!(question.contains("Answered in a message.[^m-2]"));
}

#[test]
fn the_index_lists_every_entry_and_declares_the_version() {
    let snapshot = record();
    let files = render(Format::Okf, &snapshot);
    let index = find(&files, "index.md");
    assert!(index.starts_with("---\nokf_version: \"0.2\"\n---\n"));
    assert!(index.contains("\n# Decisions\n"));
    assert!(index.contains("\n# Tasks\n"));
    assert!(index.contains("\n# Questions\n"));
    assert_eq!(index.matches("\n* [").count(), 4);
    assert!(index.contains("* [Q-4: Which sheet?](/questions/Q-4-which-sheet.md) - answered\n"));
    // Empty record.
    let empty = render(Format::Okf, &Snapshot::default());
    assert_eq!(empty.len(), 2);
    assert_eq!(
        find(&empty, "index.md"),
        "---\nokf_version: \"0.2\"\n---\n\nNo entries yet.\n"
    );
    assert!(find(&empty, "log.md").contains("No changes recorded."));
}

#[test]
fn the_log_is_newest_first_grouped_by_utc_day() {
    let snapshot = record();
    let files = render(Format::Okf, &snapshot);
    let log = find(&files, "log.md");
    assert!(
        log.starts_with("# Record change log\n\nDates are UTC. Newest first.\n\n## 2026-11-05\n\n")
    );
    let lines: Vec<&str> = log.lines().filter(|line| line.starts_with("* ")).collect();
    // Q-4 answered (600), T-3 due cleared (500), T-3 owner changed (400), Q-4 created ...
    assert!(
        lines[0].contains("**Update**") && lines[0].contains("went from open to answered"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("due date changed from 2026-11-06T00:00:00Z to none"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains("owner changed from Piper"),
        "{}",
        lines[2]
    );
    assert!(lines.last().unwrap().contains("**Creation**"));
    assert!(log.contains(
        "[D-1](/decisions/D-1-we-will-use-sqlite-for-storage.md) went from active to superseded"
    ));
    // The only heading-like lines are the two the exporter wrote.
    let headings: Vec<&str> = log.lines().filter(|line| line.starts_with('#')).collect();
    assert_eq!(headings, ["# Record change log", "## 2026-11-05"]);
}

#[test]
fn the_markdown_export_is_one_plain_file() {
    let snapshot = record();
    let files = render(Format::Md, &snapshot);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "record.md");
    let md = &files[0].content;
    assert!(md.starts_with("# Team record\n"));
    assert!(!md.starts_with("---"), "no frontmatter");
    assert!(!md.contains("[^"), "no footnotes");
    for heading in ["## Decisions", "## Tasks", "## Questions", "## History"] {
        assert!(md.contains(&format!("\n{heading}\n")), "{heading}");
    }
    assert!(md.contains("\n### D-1: We will use SQLite for storage\n\n* Status: superseded\n"));
    assert!(md.contains("* Superseded by: D-2\n"));
    assert!(md.contains("* Replaces: D-1\n"));
    assert!(md.contains("* Source: [Message in #eng"));
    assert!(md.contains("(<https://discord.com/channels/1/10/20>)"));
    assert!(md.contains("* Answered in: [Message in #eng"));
    assert!(md.contains("* 2026-11-05T00:10:00Z Q-4 went from open to answered"));
    for line in md.lines() {
        // Only the lines the exporter writes may start a heading.
        if line.starts_with('#') {
            assert!(
                line.starts_with("# Team record")
                    || line.starts_with("## ")
                    || line.starts_with("### "),
                "{line}"
            );
        }
    }
    assert_no_active_links(md);
    assert!(!md.contains("shared sheet"));
    let empty = render(Format::Md, &Snapshot::default());
    assert!(empty[0].content.contains("No entries yet."));
}

#[test]
fn rendering_twice_gives_identical_bytes() {
    let snapshot = record();
    for format in [Format::Okf, Format::Md] {
        assert_eq!(render(format, &snapshot), render(format, &snapshot));
    }
}

#[test]
fn a_deleted_source_exports_without_its_title_or_link() {
    let mut cast = hostile_cast();
    let piper = cast.piper;
    let entry = cast
        .store
        .capture_message(
            EntryKind::Decision,
            cast.first,
            piper,
            CreatedVia::Slash,
            at(100),
        )
        .unwrap()
        .entry;
    cast.store
        .apply_event(
            teamctx_core::id::SourceId::new(1).unwrap(),
            &SourceEvent::MessageDelete {
                message: MessageRef {
                    channel: sf(10),
                    message: sf(20),
                },
            },
            at(700),
        )
        .unwrap();
    let snapshot = model::gather(&cast.store).unwrap();
    for format in [Format::Okf, Format::Md] {
        for file in render(format, &snapshot) {
            // The copied title went with the deleted message.
            assert!(!file.content.contains("SQLite"), "{}", file.path);
            assert!(!file.content.contains("discord.com"), "{}", file.path);
            assert!(!file.content.contains("sources:"), "{}", file.path);
        }
    }
    let files = render(Format::Okf, &snapshot);
    let text = find(
        &files,
        &format!("decisions/{}-source-message-deleted.md", entry.short_id()),
    );
    assert!(text.contains("The source message was deleted."));
    assert!(find(&files, "log.md").contains("lost its source message to a deletion"));
}
