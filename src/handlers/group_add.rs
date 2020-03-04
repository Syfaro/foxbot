use async_trait::async_trait;
use telegram::*;

pub struct GroupAddHandler;

#[async_trait]
impl crate::Handler for GroupAddHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: Update,
        _command: Option<Command>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let message = match update.message {
            Some(message) => message,
            _ => return Ok(false),
        };

        let new_members = match &message.new_chat_members {
            Some(members) => members,
            _ => return Ok(false),
        };

        if new_members
            .iter()
            .any(|member| member.id == handler.bot_user.id)
        {
            handler.handle_welcome(message, "group-add").await;
        }

        Ok(true)
    }
}
