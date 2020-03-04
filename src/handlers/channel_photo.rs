use async_trait::async_trait;
use sentry::integrations::failure::capture_fail;
use telegram::*;

use crate::utils::{download_by_id, find_best_photo, get_message, with_user_scope};

pub struct ChannelPhotoHandler;

#[async_trait]
impl crate::Handler for ChannelPhotoHandler {
    fn name(&self) -> &'static str {
        "channel"
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

        // We only want messages from channels.
        if message.chat.chat_type != ChatType::Channel {
            return Ok(false);
        }

        let sizes = match &message.photo {
            Some(sizes) => sizes,
            _ => return Ok(false),
        };

        // We can't edit forwarded messages, so we have to ignore.
        if message.forward_date.is_some() {
            return Ok(true);
        }

        // Collect all the links in the message to see if there was a source.
        let mut links = vec![];

        // Unlikely to be text posts here, but we'll consider anyway.
        if let Some(ref text) = message.text {
            links.extend(handler.finder.links(&text));
        }

        // Links could be in an image caption.
        if let Some(ref caption) = message.caption {
            links.extend(handler.finder.links(&caption));
        }

        // See if it was posted with a bot that included an inline keyboard.
        if let Some(ref markup) = message.reply_markup {
            for row in &markup.inline_keyboard {
                for button in row {
                    if let Some(url) = &button.url {
                        links.extend(handler.finder.links(&url));
                    }
                }
            }
        }

        // TODO: this should cache file ID -> hash and their lookups.

        // Find the highest resolution size of the image and download.
        let best_photo = find_best_photo(&sizes).unwrap();
        let bytes = download_by_id(&handler.bot, &best_photo.file_id)
            .await
            .unwrap();

        // Check if we had any matches.
        let matches = match handler
            .fapi
            .image_search(&bytes, fautil::MatchType::Close)
            .await
        {
            Ok(matches) if !matches.matches.is_empty() => matches.matches,
            _ => return Ok(true),
        };

        // We know it's not empty, so get the first item and assert it's a
        // comfortable distance to be identical.
        let first = matches.first().unwrap();
        if first.distance.unwrap() > 2 {
            return Ok(true);
        }

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        if message.media_group_id.is_some() {
            let edit_caption_markup = EditMessageCaption {
                chat_id: message.chat_id(),
                message_id: Some(message.message_id),
                caption: Some(format!("https://www.furaffinity.net/view/{}/", first.id)),
                ..Default::default()
            };

            if let Err(e) = handler.bot.make_request(&edit_caption_markup).await {
                tracing::error!("unable to edit channel caption: {:?}", e);
                with_user_scope(None, None, || {
                    capture_fail(&e);
                });
            }
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

            if let Err(e) = handler.bot.make_request(&edit_reply_markup).await {
                tracing::error!("unable to edit channel reply markup: {:?}", e);
                with_user_scope(None, None, || {
                    capture_fail(&e);
                });
            }
        }

        Ok(true)
    }
}
