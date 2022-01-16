use std::sync::Arc;

use async_trait::async_trait;

mod channel_photo;
mod chosen_inline_handler;
mod commands;
mod error_cleanup;
// mod error_reply;
mod group_add;
mod group_source;
mod inline_handler;
mod permissions;
mod photo;
pub mod settings;
mod subscribe;
mod twitter;

use crate::Error;

use super::Context;

pub use channel_photo::ChannelPhotoHandler;
pub use chosen_inline_handler::ChosenInlineHandler;
pub use commands::CommandHandler;
pub use error_cleanup::ErrorCleanup;
// pub use error_reply::ErrorReplyHandler;
pub use group_add::GroupAddHandler;
pub use group_source::GroupSourceHandler;
pub use inline_handler::InlineHandler;
pub use permissions::PermissionHandler;
pub use photo::PhotoHandler;
pub use settings::SettingsHandler;
pub use subscribe::SubscribeHandler;
pub use twitter::TwitterHandler;

#[derive(Debug, PartialEq)]
pub enum Status {
    Ignored,
    Completed,
}

type WorkerEnvironment = crate::services::faktory::FaktoryWorkerEnvironment<Arc<Context>, Error>;

#[async_trait]
pub(super) trait Handler: Send + Sync {
    /// Name of the handler, for debugging/logging uses.
    fn name(&self) -> &'static str;

    fn add_jobs(&self, _worker_environment: &mut WorkerEnvironment) {}

    /// Method called for every update received.
    ///
    /// Returns if the update should be absorbed and not passed to the next handler.
    /// Errors are logged to tracing::error and reported to Sentry, if enabled.
    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error>;
}
