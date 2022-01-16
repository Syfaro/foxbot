use async_trait::async_trait;
use tgbotapi::{Command, Update};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};
use crate::{execute::telegram::Context, needs_field, Error};

pub struct GroupAddHandler;

#[async_trait]
impl Handler for GroupAddHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        let message = needs_field!(update, message);

        let new_members = match &message.new_chat_members {
            Some(members) => members,
            _ => return Ok(Ignored),
        };

        if new_members.iter().any(|member| member.id == cx.bot_user.id) {
            cx.handle_welcome(message, "group-add").await?;
        }

        Ok(Completed)
    }
}
