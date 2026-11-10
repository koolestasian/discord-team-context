//! Reading `INTERACTION_CREATE` payloads into a [`Request`] (plan section 7.3).
//!
//! The payload is read from the raw JSON, as `crate::mapping` does for messages, rather
//! than through `twilight_model::application::interaction::Interaction`: that type fails as
//! a whole when any nested object (for example a resolved message) lacks a field, and a
//! reply must still be possible then. Only the fields listed here are read; everything
//! else is ignored. Every string read here (option values, names, the resolved message)
//! is untrusted and is only ever stored or escaped for display.
//!
//! Commands are matched by exact name and option names by exact match too, so a payload
//! for a command this build does not know becomes [`Request::Unsupported`].

use std::fmt;

use serde_json::{Map, Value};
use teamctx_core::id::Snowflake;
use teamctx_core::record::EntryKind;
use teamctx_core::source::MessageRef;
use thiserror::Error;

use super::commands::{
    ANSWER, CATCHUP, DECIDE, DECISIONS, MARK_DECISION, MARK_QUESTION, MARK_TASK, QUESTION, SEARCH,
    TEAMCTX, TODO, TODO_ADD, TODO_DONE, TODO_LIST,
};
use crate::mapping::display_name;

/// The longest interaction token accepted. Discord's are a few hundred characters.
const MAX_TOKEN_BYTES: usize = 512;

/// The custom id prefix of the task modal: `task:<channel>:<message>`.
pub(crate) const TASK_MODAL_PREFIX: &str = "task:";
/// Component custom ids inside the task modal.
pub(crate) const MODAL_OWNER: &str = "owner";
pub(crate) const MODAL_DUE: &str = "due";

/// The token that lets the app answer one interaction for 15 minutes. It is a credential
/// for that reply: `Debug` hides it and it is never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct InteractionToken(String);

impl InteractionToken {
    /// Accepts 1 to 512 bytes of `[A-Za-z0-9._-]`, so the token cannot change the URL
    /// path it is placed in.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        let ok = !text.is_empty()
            && text.len() <= MAX_TOKEN_BYTES
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        ok.then(|| Self(text.to_owned()))
    }

    /// The token text, for the HTTP client only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for InteractionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InteractionToken(***)")
    }
}

/// A Discord user as the interaction describes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// User id.
    pub id: Snowflake,
    /// Server nickname, else global name, else username (untrusted).
    pub display_name: String,
    /// Bot account.
    pub is_bot: bool,
}

/// What the person asked for. Text fields are untrusted and unparsed (dates, ids and
/// queries are parsed by the handler, which owns the error messages).
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Message context menu "Mark as decision / task / question".
    Mark {
        /// The kind to record.
        kind: EntryKind,
        /// The target message.
        message: MessageRef,
        /// The resolved message object (untrusted), stored if the host lacks it.
        resolved: Value,
    },
    /// The task modal was submitted.
    TaskModal {
        /// The message the modal was opened on.
        message: MessageRef,
        /// The chosen owner, if any.
        owner: Option<Person>,
        /// The due date phrase, if any.
        due: Option<String>,
    },
    /// `/decide text [replaces]`.
    Decide {
        /// The decision.
        text: String,
        /// The decision it replaces (`D-12`).
        replaces: Option<String>,
    },
    /// `/decisions [topic] [include_superseded]`.
    Decisions {
        /// Words to match.
        topic: Option<String>,
        /// Also list superseded decisions.
        include_superseded: bool,
    },
    /// `/todo add text [owner] [due]`.
    TodoAdd {
        /// The task.
        text: String,
        /// Its owner.
        owner: Option<Person>,
        /// Its due date phrase.
        due: Option<String>,
    },
    /// `/todo list [owner] [status]`.
    TodoList {
        /// Only this owner's tasks.
        owner: Option<Person>,
        /// `open` (default), `done` or `dropped`.
        status: Option<String>,
    },
    /// `/todo done id`.
    TodoDone {
        /// The task (`T-7`).
        id: String,
    },
    /// `/question text`.
    Question {
        /// The question.
        text: String,
    },
    /// `/answer id message_link`.
    Answer {
        /// The question (`Q-3`).
        id: String,
        /// A jump link to the answering message.
        link: String,
    },
    /// `/catchup [since]`.
    Catchup {
        /// A "since" phrase.
        since: Option<String>,
    },
    /// `/search query [channel] [author]`.
    Search {
        /// Query in the plan section 9 syntax.
        query: String,
        /// Only this channel.
        channel: Option<Snowflake>,
        /// Only this author.
        author: Option<Person>,
    },
    /// `/teamctx status`.
    Status,
    /// A command, component or autocomplete this build does not handle.
    Unsupported,
}

impl Request {
    /// A short fixed name for logs (never option values).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Mark { .. } => "mark",
            Self::TaskModal { .. } => "task-modal",
            Self::Decide { .. } => "decide",
            Self::Decisions { .. } => "decisions",
            Self::TodoAdd { .. } => "todo-add",
            Self::TodoList { .. } => "todo-list",
            Self::TodoDone { .. } => "todo-done",
            Self::Question { .. } => "question",
            Self::Answer { .. } => "answer",
            Self::Catchup { .. } => "catchup",
            Self::Search { .. } => "search",
            Self::Status => "status",
            Self::Unsupported => "unsupported",
        }
    }
}

/// The parts every interaction needs for a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Interaction id (also the retry key for creating commands).
    pub id: Snowflake,
    /// The application the interaction is for.
    pub application: Snowflake,
    /// The reply token.
    pub token: InteractionToken,
}

/// One parsed interaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Interaction {
    /// Ids and token.
    pub envelope: Envelope,
    /// The server it came from; `None` in a direct message.
    pub guild: Option<Snowflake>,
    /// The channel it came from.
    pub channel: Option<Snowflake>,
    /// The person who ran it (from `member.user` in a server).
    pub invoker: Option<Person>,
    /// Their role ids (from `member.roles`).
    pub roles: Vec<Snowflake>,
    /// `public:true` was passed.
    pub public: bool,
    /// The request.
    pub request: Request,
}

/// Why a payload could not be read. Fixed text: never a value from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("an interaction payload is malformed ({0})")]
pub struct PayloadError(pub &'static str);

/// Reads the id, application id and token. Without these no reply is possible.
///
/// # Errors
///
/// [`PayloadError`] if one is missing or malformed.
pub fn envelope(data: &Value) -> Result<Envelope, PayloadError> {
    let map = data.as_object().ok_or(PayloadError("not an object"))?;
    Ok(Envelope {
        id: snowflake(map, "id").ok_or(PayloadError("id"))?,
        application: snowflake(map, "application_id").ok_or(PayloadError("application_id"))?,
        token: map
            .get("token")
            .and_then(Value::as_str)
            .and_then(InteractionToken::new)
            .ok_or(PayloadError("token"))?,
    })
}

/// Reads a whole interaction.
///
/// # Errors
///
/// [`PayloadError`] if the envelope, the invoking member or a command's options are
/// malformed.
pub fn parse(data: &Value) -> Result<Interaction, PayloadError> {
    let envelope = envelope(data)?;
    let map = data.as_object().ok_or(PayloadError("not an object"))?;
    let guild = optional_snowflake(map, "guild_id")?;
    let channel = optional_snowflake(map, "channel_id")?;
    let (invoker, roles) = match map.get("member").and_then(Value::as_object) {
        Some(member) => {
            let user = member
                .get("user")
                .and_then(Value::as_object)
                .ok_or(PayloadError("member.user"))?;
            let roles = member
                .get("roles")
                .and_then(Value::as_array)
                .map(|roles| {
                    roles
                        .iter()
                        .map(|role| {
                            role.as_str()
                                .and_then(|text| text.parse().ok())
                                .ok_or(PayloadError("member.roles"))
                        })
                        .collect::<Result<Vec<Snowflake>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            (Some(person(user, Some(member))?), roles)
        }
        None => match map.get("user").and_then(Value::as_object) {
            Some(user) => (Some(person(user, None)?), Vec::new()),
            None => (None, Vec::new()),
        },
    };
    let empty = Map::new();
    let data = map.get("data").and_then(Value::as_object).unwrap_or(&empty);
    let (request, public) = match map.get("type").and_then(Value::as_u64) {
        Some(2) => command(data)?,
        Some(5) => (modal(data)?, false),
        _ => (Request::Unsupported, false),
    };
    Ok(Interaction {
        envelope,
        guild,
        channel,
        invoker,
        roles,
        public,
        request,
    })
}

/// Application command data (`type 2`): message menus and slash commands.
fn command(data: &Map<String, Value>) -> Result<(Request, bool), PayloadError> {
    let name = data.get("name").and_then(Value::as_str).unwrap_or_default();
    let resolved = data.get("resolved").and_then(Value::as_object);
    match data.get("type").and_then(Value::as_u64) {
        // MESSAGE command.
        Some(3) => {
            let kind = match name {
                MARK_DECISION => EntryKind::Decision,
                MARK_TASK => EntryKind::Task,
                MARK_QUESTION => EntryKind::Question,
                _ => return Ok((Request::Unsupported, false)),
            };
            let target = snowflake(data, "target_id").ok_or(PayloadError("target_id"))?;
            let message = resolved
                .and_then(|r| r.get("messages"))
                .and_then(Value::as_object)
                .and_then(|messages| messages.get(&target.to_string()))
                .ok_or(PayloadError("resolved message"))?;
            let channel = message
                .as_object()
                .and_then(|m| snowflake(m, "channel_id"))
                .ok_or(PayloadError("resolved message"))?;
            Ok((
                Request::Mark {
                    kind,
                    message: MessageRef {
                        channel,
                        message: target,
                    },
                    resolved: message.clone(),
                },
                false,
            ))
        }
        // CHAT_INPUT command.
        Some(1) => {
            let top = Options::new(data.get("options"), resolved)?;
            let (options, sub) = match top.subcommand() {
                Some((sub, options)) => (Options::new(options, resolved)?, Some(sub)),
                None => (top, None),
            };
            let public = options.boolean("public")?.unwrap_or(false);
            let request = match (name, sub) {
                (DECIDE, None) => Request::Decide {
                    text: options.required_string("text")?,
                    replaces: options.string("replaces")?,
                },
                (DECISIONS, None) => Request::Decisions {
                    topic: options.string("topic")?,
                    include_superseded: options.boolean("include_superseded")?.unwrap_or(false),
                },
                (TODO, Some(TODO_ADD)) => Request::TodoAdd {
                    text: options.required_string("text")?,
                    owner: options.user("owner")?,
                    due: options.string("due")?,
                },
                (TODO, Some(TODO_LIST)) => Request::TodoList {
                    owner: options.user("owner")?,
                    status: options.string("status")?,
                },
                (TODO, Some(TODO_DONE)) => Request::TodoDone {
                    id: options.required_string("id")?,
                },
                (QUESTION, None) => Request::Question {
                    text: options.required_string("text")?,
                },
                (ANSWER, None) => Request::Answer {
                    id: options.required_string("id")?,
                    link: options.required_string("message_link")?,
                },
                (CATCHUP, None) => Request::Catchup {
                    since: options.string("since")?,
                },
                (SEARCH, None) => Request::Search {
                    query: options.required_string("query")?,
                    channel: options.channel("channel")?,
                    author: options.user("author")?,
                },
                (TEAMCTX, Some("status")) => Request::Status,
                _ => Request::Unsupported,
            };
            Ok((request, public))
        }
        _ => Ok((Request::Unsupported, false)),
    }
}

/// The task modal's submission (`type 5`).
fn modal(data: &Map<String, Value>) -> Result<Request, PayloadError> {
    let custom_id = data
        .get("custom_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(ids) = custom_id.strip_prefix(TASK_MODAL_PREFIX) else {
        return Ok(Request::Unsupported);
    };
    let (channel, message) = ids
        .split_once(':')
        .and_then(|(c, m)| Some((c.parse().ok()?, m.parse().ok()?)))
        .ok_or(PayloadError("modal custom_id"))?;
    let mut due = None;
    let mut owner_id = None;
    let mut stack: Vec<&Value> = data
        .get("components")
        .and_then(Value::as_array)
        .map(|list| list.iter().collect())
        .unwrap_or_default();
    // Labels (type 18) wrap one component; legacy action rows (type 1) hold a list.
    // The depth is bounded by the payload, which Discord limits to a few levels.
    let mut seen = 0;
    while let Some(component) = stack.pop() {
        seen += 1;
        if seen > 64 {
            return Err(PayloadError("modal components"));
        }
        let Some(map) = component.as_object() else {
            continue;
        };
        if let Some(inner) = map.get("component") {
            stack.push(inner);
        }
        if let Some(list) = map.get("components").and_then(Value::as_array) {
            stack.extend(list.iter());
        }
        match map.get("custom_id").and_then(Value::as_str) {
            Some(MODAL_DUE) => {
                due = map
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned);
            }
            Some(MODAL_OWNER) => {
                owner_id = map
                    .get("values")
                    .and_then(Value::as_array)
                    .and_then(|values| values.first())
                    .map(|value| {
                        value
                            .as_str()
                            .and_then(|text| text.parse::<Snowflake>().ok())
                            .ok_or(PayloadError("modal owner"))
                    })
                    .transpose()?;
            }
            _ => {}
        }
    }
    let resolved = data.get("resolved").and_then(Value::as_object);
    let owner = owner_id
        .map(|id| resolved_person(resolved, id))
        .transpose()?;
    Ok(Request::TaskModal {
        message: MessageRef { channel, message },
        owner,
        due,
    })
}

/// The options of one command level.
struct Options<'a> {
    list: Vec<&'a Map<String, Value>>,
    resolved: Option<&'a Map<String, Value>>,
}

impl<'a> Options<'a> {
    fn new(
        options: Option<&'a Value>,
        resolved: Option<&'a Map<String, Value>>,
    ) -> Result<Self, PayloadError> {
        let list = match options {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or(PayloadError("options"))?
                .iter()
                .map(|option| option.as_object().ok_or(PayloadError("options")))
                .collect::<Result<_, _>>()?,
        };
        Ok(Self { list, resolved })
    }

    /// A subcommand (`type 1`) and its options, if this level is one.
    fn subcommand(&self) -> Option<(&'a str, Option<&'a Value>)> {
        self.list.iter().find_map(|option| {
            (option.get("type").and_then(Value::as_u64) == Some(1)).then(|| {
                (
                    option
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    option.get("options"),
                )
            })
        })
    }

    fn value(&self, name: &str) -> Option<&'a Value> {
        self.list
            .iter()
            .find(|option| option.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|option| option.get("value"))
    }

    fn string(&self, name: &str) -> Result<Option<String>, PayloadError> {
        self.value(name)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(PayloadError("string option"))
            })
            .transpose()
    }

    fn required_string(&self, name: &str) -> Result<String, PayloadError> {
        self.string(name)?.ok_or(PayloadError("required option"))
    }

    fn boolean(&self, name: &str) -> Result<Option<bool>, PayloadError> {
        self.value(name)
            .map(|value| value.as_bool().ok_or(PayloadError("boolean option")))
            .transpose()
    }

    fn id(&self, name: &str) -> Result<Option<Snowflake>, PayloadError> {
        self.value(name)
            .map(|value| {
                value
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or(PayloadError("id option"))
            })
            .transpose()
    }

    fn channel(&self, name: &str) -> Result<Option<Snowflake>, PayloadError> {
        self.id(name)
    }

    fn user(&self, name: &str) -> Result<Option<Person>, PayloadError> {
        self.id(name)?
            .map(|id| resolved_person(self.resolved, id))
            .transpose()
    }
}

/// A user from `resolved.users` (and `resolved.members` for the nickname).
fn resolved_person(
    resolved: Option<&Map<String, Value>>,
    id: Snowflake,
) -> Result<Person, PayloadError> {
    let key = id.to_string();
    let user = resolved
        .and_then(|r| r.get("users"))
        .and_then(|users| users.get(&key))
        .and_then(Value::as_object)
        .ok_or(PayloadError("resolved user"))?;
    let member = resolved
        .and_then(|r| r.get("members"))
        .and_then(|members| members.get(&key))
        .and_then(Value::as_object);
    let person = person(user, member)?;
    if person.id != id {
        return Err(PayloadError("resolved user"));
    }
    Ok(person)
}

/// A user object, with the display-name rule applied.
fn person(
    user: &Map<String, Value>,
    member: Option<&Map<String, Value>>,
) -> Result<Person, PayloadError> {
    let id = snowflake(user, "id").ok_or(PayloadError("user id"))?;
    let username = user
        .get("username")
        .and_then(Value::as_str)
        .ok_or(PayloadError("username"))?;
    let nick = member.and_then(|m| m.get("nick")).and_then(Value::as_str);
    let global = user.get("global_name").and_then(Value::as_str);
    Ok(Person {
        id,
        display_name: display_name(nick, global, username),
        is_bot: user.get("bot").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn snowflake(map: &Map<String, Value>, key: &str) -> Option<Snowflake> {
    map.get(key)?.as_str()?.parse().ok()
}

fn optional_snowflake(
    map: &Map<String, Value>,
    key: &'static str,
) -> Result<Option<Snowflake>, PayloadError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => snowflake(map, key).map(Some).ok_or(PayloadError(key)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[expect(clippy::needless_pass_by_value, reason = "callers pass json! literals")]
    fn base(kind: u64, data: Value) -> Value {
        json!({
            "id": "1100000000000007001",
            "application_id": "1100000000000000900",
            "type": kind,
            "token": "fake-interaction-token_0123456789.abc",
            "guild_id": "1100000000000000001",
            "channel_id": "1100000000000000101",
            "member": {
                "user": { "id": "1100000000000000201", "username": "robin_example", "global_name": "Robin Example" },
                "nick": null,
                "roles": ["1100000000000000301"]
            },
            "data": data
        })
    }

    #[test]
    fn tokens_are_hidden_and_restricted() {
        let token = InteractionToken::new("abc.DEF_123-x").unwrap();
        assert_eq!(format!("{token:?}"), "InteractionToken(***)");
        for bad in ["", "a/b", "a b", "a?b", "../x", "a%2Fb", "ä"] {
            assert!(InteractionToken::new(bad).is_none(), "{bad:?}");
        }
        assert!(InteractionToken::new(&"a".repeat(513)).is_none());
    }

    #[test]
    fn slash_command_with_subcommand_and_resolved_user() {
        let data = json!({
            "id": "1100000000000000950", "name": "todo", "type": 1,
            "options": [{ "name": "add", "type": 1, "options": [
                { "name": "text", "type": 3, "value": "Write the intro" },
                { "name": "owner", "type": 6, "value": "1100000000000000202" },
                { "name": "due", "type": 3, "value": "friday" },
                { "name": "public", "type": 5, "value": true }
            ]}],
            "resolved": {
                "users": { "1100000000000000202": { "id": "1100000000000000202", "username": "sam_sample", "global_name": null } },
                "members": { "1100000000000000202": { "nick": "Sam S." } }
            }
        });
        let parsed = parse(&base(2, data)).unwrap();
        assert!(parsed.public);
        assert_eq!(parsed.roles.len(), 1);
        assert_eq!(parsed.invoker.unwrap().display_name, "Robin Example");
        match parsed.request {
            Request::TodoAdd { text, owner, due } => {
                assert_eq!(text, "Write the intro");
                assert_eq!(owner.unwrap().display_name, "Sam S.");
                assert_eq!(due.as_deref(), Some("friday"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn message_menu_reads_the_resolved_target() {
        let data = json!({
            "id": "1100000000000000951", "name": "Mark as decision", "type": 3,
            "target_id": "1100000000000001001",
            "resolved": { "messages": { "1100000000000001001": {
                "id": "1100000000000001001", "channel_id": "1100000000000000101", "content": "x"
            }}}
        });
        match parse(&base(2, data)).unwrap().request {
            Request::Mark { kind, message, .. } => {
                assert_eq!(kind, EntryKind::Decision);
                assert_eq!(message.message.get(), 1_100_000_000_000_001_001);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn modal_reads_labelled_and_legacy_components() {
        for components in [
            json!([
                { "type": 18, "id": 1, "component": { "type": 5, "id": 2, "custom_id": "owner", "values": ["1100000000000000202"] } },
                { "type": 18, "id": 3, "component": { "type": 4, "id": 4, "custom_id": "due", "value": " nov 20 " } }
            ]),
            json!([
                { "type": 1, "components": [{ "type": 4, "custom_id": "due", "value": "nov 20" }] },
                { "type": 1, "components": [{ "type": 5, "custom_id": "owner", "values": ["1100000000000000202"] }] }
            ]),
        ] {
            let data = json!({
                "custom_id": "task:1100000000000000101:1100000000000001001",
                "components": components,
                "resolved": { "users": { "1100000000000000202": { "id": "1100000000000000202", "username": "sam_sample" } } }
            });
            match parse(&base(5, data)).unwrap().request {
                Request::TaskModal {
                    message,
                    owner,
                    due,
                } => {
                    assert_eq!(message.channel.get(), 1_100_000_000_000_000_101);
                    assert_eq!(owner.unwrap().display_name, "sam_sample");
                    assert_eq!(due.as_deref(), Some("nov 20"));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn malformed_payloads_are_rejected_without_their_values() {
        let secret_text = "SECRET-CHAT-TEXT";
        let cases = [
            json!({ "id": "1", "application_id": "2", "token": secret_text.to_owned() + "/x", "type": 2 }),
            json!({ "id": secret_text, "application_id": "2", "token": "t", "type": 2 }),
            base(
                2,
                json!({ "name": "decide", "type": 1, "options": [{ "name": "text", "type": 3, "value": 5 }] }),
            ),
            base(
                2,
                json!({ "name": "Mark as task", "type": 3, "target_id": "1100000000000001001", "resolved": {} }),
            ),
            base(
                2,
                json!({ "name": "todo", "type": 1, "options": [{ "name": "list", "type": 1, "options": [{ "name": "owner", "type": 6, "value": "1100000000000000202" }] }] }),
            ),
            base(
                5,
                json!({ "custom_id": format!("task:{secret_text}:1"), "components": [] }),
            ),
        ];
        for case in cases {
            let error = parse(&case).unwrap_err();
            assert!(!error.to_string().contains(secret_text), "{error}");
        }
    }

    #[test]
    fn unknown_commands_and_other_types_are_unsupported() {
        for (kind, data) in [
            (
                2,
                json!({ "name": "ask", "type": 1, "options": [{ "name": "question", "type": 3, "value": "?" }] }),
            ),
            (2, json!({ "name": "Mark as spam", "type": 3 })),
            (
                2,
                json!({ "name": "teamctx", "type": 1, "options": [{ "name": "purge", "type": 1 }] }),
            ),
            (3, json!({ "custom_id": "x", "component_type": 2 })),
            (4, json!({ "name": "search", "type": 1 })),
            (5, json!({ "custom_id": "other", "components": [] })),
        ] {
            assert_eq!(
                parse(&base(kind, data)).unwrap().request,
                Request::Unsupported
            );
        }
    }
}
