use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::models::{GroupConfig, GroupConfigKey};
use crate::needs_field;
use crate::utils::{continuous_action, find_best_photo, match_image, sort_results, source_reply};

pub struct PhotoHandler;

#[async_trait]
impl super::Handler for PhotoHandler {
    fn name(&self) -> &'static str {
        "photo"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        let message = needs_field!(update, message);
        let photos = needs_field!(message, photo);

        if message.chat.chat_type != ChatType::Private {
            return Ok(Ignored);
        }

        let action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let best_photo = find_best_photo(&photos).unwrap();
        let mut matches = match_image(
            &handler.bot,
            &handler.conn,
            &handler.fapi,
            &best_photo,
            Some(3),
        )
        .await?;
        sort_results(
            &handler.conn,
            message.from.as_ref().unwrap().id,
            &mut matches,
        )
        .await?;

        let text = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| source_reply(&matches, &bundle),
            )
            .await;

        drop(action);

        let disable_preview = GroupConfig::get::<bool>(
            &handler.conn,
            message.chat.id,
            GroupConfigKey::GroupNoPreviews,
        )
        .await?
        .is_some();

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text,
            disable_web_page_preview: Some(disable_preview),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler
            .make_request(&send_message)
            .await
            .context("unable to send photo source reply")?;

        Ok(Completed)
    }
}
