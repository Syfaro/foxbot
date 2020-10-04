use super::Status::*;
use crate::needs_field;
use async_trait::async_trait;
use tgbotapi::*;

pub struct ChosenInlineHandler;

#[async_trait]
impl super::Handler for ChosenInlineHandler {
    fn name(&self) -> &'static str {
        "chosen"
    }

    async fn handle(
        &self,
        _handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        let _chosen_result = needs_field!(update, chosen_inline_result);

        // This doesn't need to do anything, returning completed will count it
        // as a chosen inline handler already.

        Ok(Completed)
    }
}
