use async_trait::async_trait;

use super::Status::{self, *};
use crate::models::{GroupConfig, GroupConfigKey};
use crate::needs_field;

pub struct PermissionHandler;

#[async_trait]
impl super::Handler for PermissionHandler {
    fn name(&self) -> &'static str {
        "permissions"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
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

        let data = serde_json::to_value(&my_chat_member.new_chat_member).unwrap();

        if let Err(err) = sqlx::query!(
            "INSERT INTO permission (chat_id, updated_at, permissions) VALUES ($1, to_timestamp($2::int), $3)",
            my_chat_member.chat.id,
            my_chat_member.date,
            data
        )
        .execute(&handler.conn).await {
            tracing::error!("Unable to save permission change: {:?}", err);
        }

        Ok(Completed)
    }
}
