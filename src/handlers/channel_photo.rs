use async_trait::async_trait;
use telegram::*;

use super::Status::*;
use crate::needs_field;
use crate::utils::{download_by_id, find_best_photo, get_message};

// TODO: Configuration options
// It should be possible to:
// * Link to multiple sources (change button to source name)
// * Edit messages with artist names
// * Configure localization for channel

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
    ) -> failure::Fallible<super::Status> {
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

        let first = match get_matches(&handler.bot, &handler.fapi, &sizes).await? {
            Some(first) => first,
            _ => return Ok(Completed),
        };

        // If this link was already in the message, we can ignore it.
        if link_was_seen(&extract_links(&message, &handler.finder), &first.url) {
            return Ok(Completed);
        }

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        if message.media_group_id.is_some() {
            let edit_caption_markup = EditMessageCaption {
                chat_id: message.chat_id(),
                message_id: Some(message.message_id),
                caption: Some(first.url()),
                ..Default::default()
            };

            handler.bot.make_request(&edit_caption_markup).await?;
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

            handler.bot.make_request(&edit_reply_markup).await?;
        }

        Ok(Completed)
    }
}

/// Extract all possible links from a Message. It looks at the text,
/// caption, and all buttons within an inline keyboard.
fn extract_links<'m>(message: &'m Message, finder: &linkify::LinkFinder) -> Vec<linkify::Link<'m>> {
    let mut links = vec![];

    // Unlikely to be text posts here, but we'll consider anyway.
    if let Some(ref text) = message.text {
        links.extend(finder.links(&text));
    }

    // Links could be in an image caption.
    if let Some(ref caption) = message.caption {
        links.extend(finder.links(&caption));
    }

    // See if it was posted with a bot that included an inline keyboard.
    if let Some(ref markup) = message.reply_markup {
        for row in &markup.inline_keyboard {
            for button in row {
                if let Some(url) = &button.url {
                    links.extend(finder.links(&url));
                }
            }
        }
    }

    links
}

/// Check if a link was contained within a linkify Link.
fn link_was_seen(links: &[linkify::Link], source: &str) -> bool {
    links.iter().any(|link| link.as_str() == source)
}

async fn get_matches(
    bot: &Telegram,
    fapi: &fautil::FAUtil,
    sizes: &[PhotoSize],
) -> reqwest::Result<Option<fautil::File>> {
    // Find the highest resolution size of the image and download.
    let best_photo = find_best_photo(&sizes).unwrap();
    let bytes = download_by_id(&bot, &best_photo.file_id).await.unwrap();

    // Run an image search for these bytes, get the first result.
    fapi.image_search(&bytes, fautil::MatchType::Close)
        .await
        .map(|matches| matches.matches.into_iter().next())
}

#[cfg(test)]
mod tests {
    fn get_finder() -> linkify::LinkFinder {
        let mut finder = linkify::LinkFinder::new();
        finder.kinds(&[linkify::LinkKind::Url]);

        finder
    }

    #[test]
    fn test_find_links() {
        let finder = get_finder();

        let expected_links = vec![
            "https://syfaro.net",
            "https://huefox.com",
            "https://e621.net",
            "https://www.furaffinity.net",
        ];

        let message = telegram::Message {
            text: Some(
                "My message has a link like this: https://syfaro.net and some words after it."
                    .into(),
            ),
            caption: Some("There can also be links in the caption: https://huefox.com".into()),
            reply_markup: Some(telegram::InlineKeyboardMarkup {
                inline_keyboard: vec![
                    vec![telegram::InlineKeyboardButton {
                        url: Some("https://e621.net".into()),
                        ..Default::default()
                    }],
                    vec![telegram::InlineKeyboardButton {
                        url: Some("https://www.furaffinity.net".into()),
                        ..Default::default()
                    }],
                ],
            }),
            ..Default::default()
        };

        let links = super::extract_links(&message, &finder);

        assert_eq!(
            links.len(),
            expected_links.len(),
            "found different number of links"
        );

        for (link, expected) in links.iter().zip(expected_links.iter()) {
            assert_eq!(&link.as_str(), expected);
        }
    }

    #[test]
    fn test_link_was_seen() {
        let finder = get_finder();

        let test = "https://www.furaffinity.net/";
        let found_links = finder.links(&test);

        let mut links = vec![];
        links.extend(found_links);

        assert!(
            super::link_was_seen(&links, "https://www.furaffinity.net/"),
            "seen link was not found"
        );

        assert!(
            !super::link_was_seen(&links, "https://e621.net/"),
            "unseen link was found"
        );
    }
}
