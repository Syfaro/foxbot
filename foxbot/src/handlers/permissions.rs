use async_trait::async_trait;
use tgbotapi::ChatMemberUpdated;

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_models::{ChatAdmin, GroupConfig, GroupConfigKey, Permissions};

pub struct PermissionHandler;

#[async_trait]
impl Handler for PermissionHandler {
    fn name(&self) -> &'static str {
        "permissions"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> anyhow::Result<Status> {
        if handle_my_chat_member(&handler, &update.my_chat_member).await? {
            handle_chat_member(&handler, &update.my_chat_member).await?;
            return Ok(Completed);
        }

        if handle_chat_member(&handler, &update.chat_member).await? {
            return Ok(Completed);
        }

        Ok(Ignored)
    }
}

async fn handle_my_chat_member(
    handler: &MessageHandler,
    my_chat_member: &Option<ChatMemberUpdated>,
) -> anyhow::Result<bool> {
    let my_chat_member = match my_chat_member {
        Some(my_chat_member) => my_chat_member,
        _ => return Ok(false),
    };

    tracing::info!("Got updated my chat member info: {:?}", my_chat_member);

    let can_delete = my_chat_member
        .new_chat_member
        .can_delete_messages
        .unwrap_or(false);

    if let Err(err) = GroupConfig::set(
        &handler.conn,
        GroupConfigKey::HasDeletePermission,
        my_chat_member.chat.id,
        can_delete,
    )
    .await
    {
        tracing::error!("Unable to set group delete permission: {:?}", err);
    }

    if let Err(err) = Permissions::add_change(&handler.conn, my_chat_member).await {
        tracing::error!("Unable to save permission change: {:?}", err);
    }

    Ok(true)
}

async fn handle_chat_member(
    handler: &MessageHandler,
    chat_member: &Option<ChatMemberUpdated>,
) -> anyhow::Result<bool> {
    let chat_member = match chat_member {
        Some(chat_member) => chat_member,
        _ => return Ok(false),
    };

    tracing::debug!("Got updated chat member info: {:?}", chat_member);

    if let Err(err) = ChatAdmin::update_chat(&handler.conn, &chat_member).await {
        tracing::error!("Unable to save permission change: {:?}", err);
    }

    Ok(true)
}
