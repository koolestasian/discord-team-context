//! Slash commands and message context menus: the bot's only "voice" (plan section 7.3,
//! ADR 0005).
//!
//! Layers, each tested on its own:
//!
//! - [`commands`]: what is registered on start (guild commands, one bulk overwrite).
//! - [`payload`]: `INTERACTION_CREATE` JSON into a [`payload::Request`].
//! - [`handler`]: the pure command logic against the store, producing an
//!   [`handler::Outcome`] (record writes go through `RecordEdit`, which writes the audit
//!   log).
//! - [`render`] and [`access`]: escaping and length limits, and who may see what.
//! - [`service`]: the async transport. It answers each interaction once through
//!   `twilight-http`'s interaction client (`crate::http`), defers slow ones and then edits
//!   the original response. No follow-up messages, no deletes, nothing else.
//!
//! Every reply is ephemeral unless the person passed `public:true`, and every reply sets
//! `allowed_mentions` to parse nothing.

pub mod access;
pub mod commands;
pub mod handler;
pub mod payload;
pub mod render;
pub mod service;

#[cfg(test)]
mod tests;

use teamctx_core::source::MessageRef;
use twilight_model::channel::message::MessageFlags;
use twilight_model::channel::message::component::{
    Component, Label, SelectMenu, SelectMenuType, TextInput, TextInputStyle,
};
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};

pub use handler::{Outcome, Reply};
pub use service::{
    InteractionApi, InteractionConfig, InteractionEvent, InteractionService, ServiceTiming,
};

use payload::{MODAL_DUE, MODAL_OWNER, TASK_MODAL_PREFIX};

impl Reply {
    /// The message data: content and embeds within Discord's limits, the ephemeral flag
    /// unless public, and mentions that cannot ping.
    #[must_use]
    pub fn data(&self) -> InteractionResponseData {
        let content = render::clip_fixed(&self.content, render::MAX_CONTENT);
        InteractionResponseData {
            allowed_mentions: Some(render::no_mentions()),
            content: (!content.is_empty()).then_some(content),
            embeds: (!self.embeds.is_empty()).then(|| render::fit_embeds(self.embeds.clone())),
            flags: flags(self.public),
            ..InteractionResponseData::default()
        }
    }
}

impl Outcome {
    /// The initial response (callback type 4, or 9 for the task modal).
    #[must_use]
    pub fn response(&self) -> InteractionResponse {
        match self {
            Self::Reply(reply) => InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(reply.data()),
            },
            Self::Modal(message) => task_modal(*message),
        }
    }
}

fn flags(public: bool) -> Option<MessageFlags> {
    (!public).then_some(MessageFlags::EPHEMERAL)
}

/// "Thinking…" (callback type 5); the reply follows as an edit of the original response.
#[must_use]
pub fn deferred(public: bool) -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::DeferredChannelMessageWithSource,
        data: Some(InteractionResponseData {
            flags: flags(public),
            ..InteractionResponseData::default()
        }),
    }
}

/// The "Mark as task" modal: an optional owner (a user picker, so no name guessing) and an
/// optional due date, each in a Label component (Discord deprecated action rows with text
/// inputs in modals). Its custom id carries the message, checked again on submit.
#[must_use]
#[expect(
    deprecated,
    reason = "TextInput::label must still be named; it is left empty in favour of Label"
)]
pub fn task_modal(message: MessageRef) -> InteractionResponse {
    let owner = Component::SelectMenu(SelectMenu {
        channel_types: None,
        custom_id: MODAL_OWNER.to_owned(),
        default_values: None,
        disabled: false,
        id: None,
        kind: SelectMenuType::User,
        max_values: Some(1),
        min_values: Some(0),
        options: None,
        placeholder: Some("Nobody yet".to_owned()),
        required: Some(false),
    });
    let due = Component::TextInput(TextInput {
        custom_id: MODAL_DUE.to_owned(),
        id: None,
        label: None,
        max_length: Some(64),
        min_length: None,
        placeholder: Some("friday, nov 20 or 2026-11-20".to_owned()),
        required: Some(false),
        style: TextInputStyle::Short,
        value: None,
    });
    let label = |text: &str, description: &str, component| {
        Component::Label(Label {
            component: Box::new(component),
            description: Some(description.to_owned()),
            id: None,
            label: text.to_owned(),
        })
    };
    InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(InteractionResponseData {
            custom_id: Some(format!(
                "{TASK_MODAL_PREFIX}{}:{}",
                message.channel, message.message
            )),
            title: Some("Mark as task".to_owned()),
            components: Some(vec![
                label("Owner", "Optional", owner),
                label("Due date", "Optional; dates use the host's time zone", due),
            ]),
            ..InteractionResponseData::default()
        }),
    }
}
