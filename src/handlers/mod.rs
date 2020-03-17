use async_trait::async_trait;

mod channel_photo;
mod chosen_inline_handler;
mod commands;
mod error_reply;
mod group_add;
mod group_source;
mod inline_handler;
mod photo;
pub mod settings;
mod text;

pub use channel_photo::ChannelPhotoHandler;
pub use chosen_inline_handler::ChosenInlineHandler;
pub use commands::CommandHandler;
pub use error_reply::ErrorReplyHandler;
pub use group_add::GroupAddHandler;
pub use group_source::GroupSourceHandler;
pub use inline_handler::InlineHandler;
pub use photo::PhotoHandler;
pub use settings::SettingsHandler;
pub use text::TextHandler;

#[derive(PartialEq)]
pub enum Status {
    Ignored,
    Completed,
}

#[async_trait]
pub trait Handler: Send + Sync {
    /// Name of the handler, for debugging/logging uses.
    fn name(&self) -> &'static str;

    /// Method called for every update received.
    ///
    /// Returns if the update should be absorbed and not passed to the next handler.
    /// Errors are logged to log::error and reported to Sentry, if enabled.
    async fn handle(
        &self,
        handler: &super::MessageHandler,
        update: &tgbotapi::Update,
        command: Option<&tgbotapi::Command>,
    ) -> failure::Fallible<Status>;
}

#[macro_export]
macro_rules! needs_field {
    ($message:expr, $field:tt) => {
        match $message.$field {
            Some(ref field) => field,
            _ => return Ok(crate::handlers::Status::Ignored),
        }
    };
}
