use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::needs_field;
use crate::utils::{extract_links, get_matches, get_message, link_was_seen};

pub struct ChannelPhotoHandler;

#[async_trait]
impl super::Handler for ChannelPhotoHandler {
    fn name(&self) -> &'static str {
        "channel"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        // Ensure we have a channel_post Message and a photo within.
        let message = needs_field!(update, channel_post);
        let sizes = needs_field!(&message, photo);

        // We only want messages from channels. I think this is always true
        // because this came from a channel_post.
        if message.chat.chat_type != ChatType::Channel {
            return Ok(Ignored);
        }

        // We can't edit forwarded messages, so we have to ignore.
        if message.forward_date.is_some() {
            return Ok(Completed);
        }

        let matches = get_matches(&handler.bot, &handler.fapi, &handler.conn, &sizes)
            .await
            .context("unable to get matches")?;

        let first = match matches {
            Some(first) => first,
            _ => return Ok(Completed),
        };

        // Ignore unlikely matches
        if first.distance.unwrap() > 3 {
            return Ok(Completed);
        }

        let sites = handler.sites.lock().await;

        // If this link was already in the message, we can ignore it.
        if link_was_seen(&sites, &extract_links(&message), &first.url) {
            return Ok(Completed);
        }

        drop(sites);

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        if message.media_group_id.is_some() {
            let edit_caption_markup = EditMessageCaption {
                chat_id: message.chat_id(),
                message_id: Some(message.message_id),
                caption: Some(first.url()),
                ..Default::default()
            };

            handler
                .make_request(&edit_caption_markup)
                .await
                .context("unable to edit channel caption markup")?;
        // Not a media group, we should create an inline keyboard.
        } else {
            let text = handler
                .get_fluent_bundle(None, |bundle| {
                    get_message(&bundle, "inline-source", None).unwrap()
                })
                .await;

            let markup = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text,
                    url: Some(first.url()),
                    ..Default::default()
                }]],
            };

            let edit_reply_markup = EditMessageReplyMarkup {
                chat_id: message.chat_id(),
                message_id: Some(message.message_id),
                reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(markup)),
                ..Default::default()
            };

            handler
                .make_request(&edit_reply_markup)
                .await
                .context("unable to edit channel reply markup")?;
        }

        Ok(Completed)
    }
}
