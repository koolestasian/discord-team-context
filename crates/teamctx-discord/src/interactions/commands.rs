//! The commands registered with Discord on start (plan section 7.3).
//!
//! Three message context menus and eight slash commands, registered as guild commands
//! (they update instantly) with one bulk overwrite. Every slash command takes an optional
//! `public` flag; replies are ephemeral without it.
//!
//! `/ask` is deliberately absent: it needs the LLM pipeline (plan section 11, milestone
//! M6). When that exists it is added to [`commands`] behind the `[llm]` configuration,
//! and `ask_is_not_registered` below changes with it.
//!
//! Limits checked against Discord's documentation (`docs/research/M5.md`): names of
//! slash commands and options are lowercase, 1 to 32 characters; message command names
//! may contain spaces and capitals; descriptions 1 to 100 characters; at most 25 options;
//! required options come first.

use std::collections::HashMap;

use twilight_model::application::command::{
    Command, CommandOption, CommandOptionChoice, CommandOptionChoiceValue, CommandOptionType,
    CommandType,
};
use twilight_model::application::interaction::InteractionContextType;
use twilight_model::channel::ChannelType;
use twilight_model::id::Id;

/// Message context menu names.
pub(crate) const MARK_DECISION: &str = "Mark as decision";
pub(crate) const MARK_TASK: &str = "Mark as task";
pub(crate) const MARK_QUESTION: &str = "Mark as question";

/// Slash command names.
pub(crate) const DECIDE: &str = "decide";
pub(crate) const DECISIONS: &str = "decisions";
pub(crate) const TODO: &str = "todo";
pub(crate) const TODO_ADD: &str = "add";
pub(crate) const TODO_LIST: &str = "list";
pub(crate) const TODO_DONE: &str = "done";
pub(crate) const QUESTION: &str = "question";
pub(crate) const ANSWER: &str = "answer";
pub(crate) const CATCHUP: &str = "catchup";
pub(crate) const SEARCH: &str = "search";
pub(crate) const TEAMCTX: &str = "teamctx";

/// Longest title a person can type (plan section 6: titles are at most 200 characters).
const TITLE_MAX: u16 = 200;
/// Short ids such as `D-12`.
const ID_MAX: u16 = 24;
/// Date phrases (`core::when` accepts at most 64 bytes).
const DATE_MAX: u16 = 64;
/// Search queries (the parser accepts at most 2,000 bytes).
const QUERY_MAX: u16 = 500;
/// Topics and message links.
const SHORT_MAX: u16 = 200;

/// Every command this build registers.
#[must_use]
pub fn commands() -> Vec<Command> {
    let public = || boolean("public", "Show the reply to everyone in the channel");
    vec![
        message_menu(MARK_DECISION),
        message_menu(MARK_TASK),
        message_menu(MARK_QUESTION),
        slash(
            DECIDE,
            "Record a decision",
            vec![
                string("text", "What was decided", true, TITLE_MAX),
                string(
                    "replaces",
                    "The decision this replaces, like D-12",
                    false,
                    ID_MAX,
                ),
                public(),
            ],
        ),
        slash(
            DECISIONS,
            "List current decisions",
            vec![
                string("topic", "Words to match", false, SHORT_MAX),
                boolean("include_superseded", "Also list replaced decisions"),
                public(),
            ],
        ),
        slash(
            TODO,
            "Track tasks",
            vec![
                subcommand(
                    TODO_ADD,
                    "Add a task",
                    vec![
                        string("text", "The task", true, TITLE_MAX),
                        user("owner", "Who owns it"),
                        string(
                            "due",
                            "Due date, like friday, nov 20 or 2026-11-20",
                            false,
                            DATE_MAX,
                        ),
                        public(),
                    ],
                ),
                subcommand(
                    TODO_LIST,
                    "List tasks",
                    vec![
                        user("owner", "Only this person's tasks"),
                        choice(
                            "status",
                            "Which tasks (default: open)",
                            &["open", "done", "dropped"],
                        ),
                        public(),
                    ],
                ),
                subcommand(
                    TODO_DONE,
                    "Mark a task done",
                    vec![string("id", "The task, like T-7", true, ID_MAX), public()],
                ),
            ],
        ),
        slash(
            QUESTION,
            "Record an open question",
            vec![string("text", "The question", true, TITLE_MAX), public()],
        ),
        slash(
            ANSWER,
            "Mark a question answered by a message",
            vec![
                string("id", "The question, like Q-3", true, ID_MAX),
                string(
                    "message_link",
                    "Link to the answering message (Copy Message Link)",
                    true,
                    SHORT_MAX,
                ),
                public(),
            ],
        ),
        slash(
            CATCHUP,
            "What changed since you last caught up",
            vec![
                string(
                    "since",
                    "Like 24h, 3d, yesterday or 2026-11-01 (at most 14 days)",
                    false,
                    DATE_MAX,
                ),
                public(),
            ],
        ),
        slash(
            SEARCH,
            "Search the team's messages",
            vec![
                string(
                    "query",
                    "Words, \"phrases\", from:name, in:channel, after:2026-01-01",
                    true,
                    QUERY_MAX,
                ),
                channel("channel", "Only this channel"),
                user("author", "Only messages by this person"),
                public(),
            ],
        ),
        slash(
            TEAMCTX,
            "About this teamctx host",
            vec![subcommand(
                "status",
                "Sync status of this host",
                vec![public()],
            )],
        ),
    ]
}

#[expect(
    deprecated,
    reason = "`dm_permission` must still be named; contexts replaces it"
)]
fn command(
    kind: CommandType,
    name: &str,
    description: &str,
    options: Vec<CommandOption>,
) -> Command {
    Command {
        application_id: None,
        contexts: Some(vec![InteractionContextType::Guild]),
        default_member_permissions: None,
        description: description.to_owned(),
        description_localizations: None,
        dm_permission: None,
        guild_id: None,
        id: None,
        integration_types: None,
        kind,
        name: name.to_owned(),
        name_localizations: None,
        nsfw: None,
        options,
        version: Id::new(1),
    }
}

fn message_menu(name: &str) -> Command {
    command(CommandType::Message, name, "", Vec::new())
}

fn slash(name: &str, description: &str, options: Vec<CommandOption>) -> Command {
    command(CommandType::ChatInput, name, description, options)
}

fn option(kind: CommandOptionType, name: &str, description: &str, required: bool) -> CommandOption {
    CommandOption {
        autocomplete: None,
        channel_types: None,
        choices: None,
        description: description.to_owned(),
        description_localizations: None,
        kind,
        max_length: None,
        max_value: None,
        min_length: None,
        min_value: None,
        name: name.to_owned(),
        name_localizations: None,
        options: None,
        file_types: None,
        required: required.then_some(true),
    }
}

fn string(name: &str, description: &str, required: bool, max: u16) -> CommandOption {
    CommandOption {
        max_length: Some(max),
        min_length: Some(1),
        ..option(CommandOptionType::String, name, description, required)
    }
}

fn boolean(name: &str, description: &str) -> CommandOption {
    option(CommandOptionType::Boolean, name, description, false)
}

fn user(name: &str, description: &str) -> CommandOption {
    option(CommandOptionType::User, name, description, false)
}

fn channel(name: &str, description: &str) -> CommandOption {
    CommandOption {
        channel_types: Some(vec![
            ChannelType::GuildText,
            ChannelType::GuildAnnouncement,
            ChannelType::GuildForum,
            ChannelType::PublicThread,
            ChannelType::PrivateThread,
            ChannelType::AnnouncementThread,
        ]),
        ..option(CommandOptionType::Channel, name, description, false)
    }
}

fn choice(name: &str, description: &str, values: &[&str]) -> CommandOption {
    CommandOption {
        choices: Some(
            values
                .iter()
                .map(|value| CommandOptionChoice {
                    name: (*value).to_owned(),
                    name_localizations: None::<HashMap<String, String>>,
                    value: CommandOptionChoiceValue::String((*value).to_owned()),
                })
                .collect(),
        ),
        ..option(CommandOptionType::String, name, description, false)
    }
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOption>) -> CommandOption {
    CommandOption {
        options: Some(options),
        ..option(CommandOptionType::SubCommand, name, description, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_name(name: &str) -> bool {
        (1..=32).contains(&name.chars().count())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    }

    fn check_options(options: &[CommandOption]) {
        assert!(options.len() <= 25);
        let mut optional_seen = false;
        for option in options {
            assert!(valid_name(&option.name), "{}", option.name);
            assert!(
                (1..=100).contains(&option.description.chars().count()),
                "{}",
                option.name
            );
            if option.kind == CommandOptionType::SubCommand {
                check_options(option.options.as_deref().unwrap_or_default());
                continue;
            }
            let required = option.required == Some(true);
            assert!(
                !(required && optional_seen),
                "required after optional: {}",
                option.name
            );
            optional_seen |= !required;
            if let Some(max) = option.max_length {
                assert!(max <= 6000);
            }
        }
    }

    #[test]
    fn the_planned_commands_are_registered_and_valid() {
        let commands = commands();
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                MARK_DECISION,
                MARK_TASK,
                MARK_QUESTION,
                DECIDE,
                DECISIONS,
                TODO,
                QUESTION,
                ANSWER,
                CATCHUP,
                SEARCH,
                TEAMCTX
            ]
        );
        for command in &commands {
            match command.kind {
                CommandType::Message => {
                    assert!((1..=32).contains(&command.name.chars().count()));
                    assert!(command.description.is_empty() && command.options.is_empty());
                }
                CommandType::ChatInput => {
                    assert!(valid_name(&command.name), "{}", command.name);
                    assert!((1..=100).contains(&command.description.chars().count()));
                    check_options(&command.options);
                }
                other => panic!("unexpected kind {other:?}"),
            }
            assert_eq!(command.contexts, Some(vec![InteractionContextType::Guild]));
            assert_eq!(command.default_member_permissions, None);
        }
    }

    #[test]
    fn ask_is_not_registered() {
        // `/ask` needs the LLM pipeline (M6); until then it must not appear.
        assert!(commands().iter().all(|c| c.name != "ask"));
    }

    #[test]
    fn every_slash_command_takes_public() {
        fn has_public(options: &[CommandOption]) -> bool {
            options.iter().any(|o| o.name == "public")
                || options.iter().any(|o| {
                    o.kind == CommandOptionType::SubCommand
                        && o.options.as_deref().is_some_and(has_public)
                })
        }
        for command in commands()
            .iter()
            .filter(|c| c.kind == CommandType::ChatInput)
        {
            assert!(has_public(&command.options), "{}", command.name);
        }
    }

    #[test]
    fn serialised_shape_matches_discord() {
        let json = serde_json::to_value(commands()).unwrap();
        assert_eq!(json[0]["type"], 3);
        assert_eq!(json[0]["name"], MARK_DECISION);
        assert_eq!(json[3]["type"], 1);
        assert_eq!(json[3]["options"][0]["type"], 3);
        assert_eq!(json[3]["options"][0]["required"], true);
        assert_eq!(json[3]["contexts"], serde_json::json!([0]));
        assert_eq!(json[5]["options"][0]["type"], 1, "todo add is a subcommand");
    }
}
