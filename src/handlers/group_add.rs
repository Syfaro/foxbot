use super::Status::*;
use crate::needs_field;
use async_trait::async_trait;
use tgbotapi::*;

pub struct GroupAddHandler;

#[async_trait]
impl super::Handler for GroupAddHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<super::Status, failure::Error> {
        let message = needs_field!(update, message);

        let new_members = match &message.new_chat_members {
            Some(members) => members,
            _ => return Ok(Ignored),
        };

        if new_members
            .iter()
            .any(|member| member.id == handler.bot_user.id)
        {
            handler.handle_welcome(message, "group-add").await?;
        }

        Ok(Completed)
    }
}
