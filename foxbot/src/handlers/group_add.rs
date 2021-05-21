use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{Command, Update};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};
use crate::MessageHandler;
use foxbot_utils::needs_field;

pub struct GroupAddHandler;

#[async_trait]
impl Handler for GroupAddHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        let message = needs_field!(update, message);

        let new_members = match &message.new_chat_members {
            Some(members) => members,
            _ => return Ok(Ignored),
        };

        if new_members
            .iter()
            .any(|member| member.id == handler.bot_user.id)
        {
            handler
                .handle_welcome(message, "group-add")
                .await
                .context("unable to send group add welcome message")?;
        }

        Ok(Completed)
    }
}
