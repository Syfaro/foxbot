use async_trait::async_trait;

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_models::{GroupConfig, GroupConfigKey, Permissions};
use foxbot_utils::*;

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
        let my_chat_member = needs_field!(update, my_chat_member);

        tracing::info!(new_chat_member = ?my_chat_member.new_chat_member, "Got updated chat member info");

        let can_delete = my_chat_member
            .new_chat_member
            .can_delete_messages
            .unwrap_or(false);

        GroupConfig::set(
            &handler.conn,
            GroupConfigKey::HasDeletePermission,
            my_chat_member.chat.id,
            can_delete,
        )
        .await?;

        if let Err(err) = Permissions::add_change(&handler.conn, my_chat_member).await {
            tracing::error!("Unable to save permission change: {:?}", err);
        }

        Ok(Completed)
    }
}
