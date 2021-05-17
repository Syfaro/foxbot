use anyhow::Context;
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

    match ChatAdmin::update_chat(
        &handler.conn,
        &chat_member.new_chat_member.status,
        chat_member.new_chat_member.user.id,
        chat_member.chat.id,
        chat_member.date,
    )
    .await
    {
        Ok(is_admin) => {
            tracing::debug!(
                user_id = chat_member.new_chat_member.user.id,
                is_admin,
                "Updated user"
            );

            // Handle loading some data when the bot's administrative status
            // changes.
            if chat_member.new_chat_member.user.id == handler.bot_user.id {
                if let Err(err) = handle_bot_update(&handler, &chat_member, is_admin).await {
                    tracing::error!("Unable to update requested data: {:?}", err);
                }
            }
        }
        Err(err) => {
            tracing::error!("Unable to save permission change: {:?}", err);
        }
    }

    Ok(true)
}

async fn handle_bot_update(
    handler: &MessageHandler,
    chat_member: &ChatMemberUpdated,
    is_admin: bool,
) -> anyhow::Result<()> {
    tracing::debug!("Bot permissions changed");

    if !is_admin {
        tracing::warn!("Bot has lost admin permissions, discarding potentially stale data");

        ChatAdmin::flush(&handler.conn, handler.bot_user.id, chat_member.chat.id)
            .await
            .context("Bot permissions changed and unable to flush channel administrators")?;
    } else {
        let get_chat_administrators = tgbotapi::requests::GetChatAdministrators {
            chat_id: chat_member.chat.id.into(),
        };

        let admins = handler
            .make_request(&get_chat_administrators)
            .await
            .context("Unable to get chat administrators after bot permissions changed")?;

        for admin in admins {
            tracing::trace!(user_id = admin.user.id, "Discovered group admin");

            ChatAdmin::update_chat(
                &handler.conn,
                &admin.status,
                admin.user.id,
                chat_member.chat.id,
                0,
            )
            .await
            .context(
                "Unable to update administrator status of user after bot permissions changed",
            )?;
        }
    }

    Ok(())
}
