use anyhow::Context;
use async_trait::async_trait;
use redis::AsyncCommands;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::needs_field;
use crate::utils::{extract_links, find_best_photo, get_message, link_was_seen, match_image};

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

        // See later comment, but this likely means we can't edit this message.
        // Might be worth testing more in the future, but for now, we should
        // just ignore it.
        if message.reply_markup.is_some() {
            return Ok(Completed);
        }

        let file = find_best_photo(&sizes).unwrap();
        let mut matches = match_image(&handler.bot, &handler.conn, &handler.fapi, &file)
            .await
            .context("unable to get matches")?;

        // Only keep matches with a distance of 3 or less
        matches.retain(|m| m.distance.unwrap() <= 3);

        let first = match matches.first() {
            Some(first) => first,
            _ => return Ok(Completed),
        };

        let sites = handler.sites.lock().await;

        // If this link was already in the message, we can ignore it.
        if link_was_seen(&sites, &extract_links(&message), &first.url) {
            return Ok(Completed);
        }

        drop(sites);

        // Telegram only shows a caption on a media group if there is a single
        // caption anywhere in the group. When users upload a group, we need
        // to check if we can only set a single source to make the link more
        // visible. This can be done by ensuring our source has been previouly
        // used in the media group.
        //
        // For our implementation, this is done by maintaining a Redis set of
        // every source previously displayed. If adding our source links returns
        // fewer inserted than we had, it means a link was previously used and
        // therefore we do not have to set a source.
        //
        // Because Telegram doesn't send media groups at once, we have to store
        // these values until we're sure the group is over. In this case, we
        // will store values for 300 seconds.
        //
        // No link normalization is required here because all links are already
        // normalized when coming from FuzzySearch.
        if let Some(group_id) = &message.media_group_id {
            let key = format!("group-sources:{}", group_id);

            let mut urls = matches.iter().map(|m| m.url()).collect::<Vec<_>>();
            urls.sort();
            urls.dedup();
            let source_count = urls.len();

            let mut conn = handler.redis.clone();
            let added_links: usize = conn.sadd(&key, urls).await?;
            conn.expire(&key, 300).await?;

            if source_count > added_links {
                tracing::debug!(
                    source_count,
                    added_links,
                    "media group already contained source"
                );
                return Ok(Completed);
            }
        }

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        let resp = if message.media_group_id.is_some() {
            let edit_caption_markup = EditMessageCaption {
                chat_id: message.chat_id(),
                message_id: Some(message.message_id),
                caption: Some(first.url()),
                ..Default::default()
            };

            handler.make_request(&edit_caption_markup).await
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

            handler.make_request(&edit_reply_markup).await
        };

        match resp {
            // It seems like the bot gets updates from channels, but is unable
            // to update them. Telegram often gives us a
            // 'Bad Request: MESSAGE_ID_INVALID' response here.
            //
            // The corresponding updates have inline keyboard markup, suggesting
            // that they were generated by a bot.
            //
            // I'm not sure if there's any way to detect this before processing
            // an update, so ignore these errors.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(400),
                ..
            })) => Ok(Completed),
            Ok(_) => Ok(Completed),
            Err(e) => Err(e).context("unable to update channel message"),
        }
    }
}
