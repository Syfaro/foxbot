use async_trait::async_trait;
use tgbotapi::{Command, Update};

use super::{
    Handler,
    Status::{self, *},
};
use crate::{execute::telegram::Context, needs_field, Error};

pub struct ChosenInlineHandler;

#[async_trait]
impl Handler for ChosenInlineHandler {
    fn name(&self) -> &'static str {
        "chosen"
    }

    async fn handle(
        &self,
        _context: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        let _chosen_result = needs_field!(update, chosen_inline_result);

        // This doesn't need to do anything, returning completed will count it
        // as a chosen inline handler already.

        Ok(Completed)
    }
}
