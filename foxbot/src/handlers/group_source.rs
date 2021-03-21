use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_models::{GroupConfig, GroupConfigKey};
use foxbot_utils::*;

const MAX_SOURCE_DISTANCE: u64 = 3;
const NOISY_SOURCE_COUNT: usize = 4;

pub struct GroupSourceHandler;

#[async_trait]
impl Handler for GroupSourceHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        let message = needs_field!(update, message);
        let photo_sizes = needs_field!(message, photo);

        match GroupConfig::get(&handler.conn, message.chat.id, GroupConfigKey::GroupAdd)
            .await
            .context("unable to query group add config")?
        {
            Some(val) if val => (),
            _ => return Ok(Ignored),
        }

        let links = extract_links(&message);
        let action = if links.is_empty() {
            Some(continuous_action(
                handler.bot.clone(),
                6,
                message.chat_id(),
                message.from.clone(),
                ChatAction::Typing,
            ))
        } else {
            None
        };

        let best_photo = find_best_photo(&photo_sizes).unwrap();
        let mut matches = match_image(
            &handler.bot,
            &handler.conn,
            &handler.fapi,
            &best_photo,
            Some(3),
        )
        .await?
        .1;
        sort_results(
            &handler.conn,
            message.from.as_ref().unwrap().id,
            &mut matches,
        )
        .await?;

        let wanted_matches = matches
            .iter()
            .filter(|m| m.distance.unwrap() <= MAX_SOURCE_DISTANCE)
            .collect::<Vec<_>>();

        if wanted_matches.is_empty() {
            return Ok(Completed);
        }

        let sites = handler.sites.lock().await;

        if wanted_matches
            .iter()
            .any(|m| link_was_seen(&sites, &links, &m.url()))
        {
            return Ok(Completed);
        }

        drop(sites);

        // Prevents memes from getting a million links in chat
        if wanted_matches.len() >= NOISY_SOURCE_COUNT {
            tracing::trace!(
                count = wanted_matches.len(),
                "had too many matches, ignoring"
            );
            return Ok(Completed);
        }

        let lang = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.as_deref());

        let text = handler
            .get_fluent_bundle(lang, |bundle| {
                if wanted_matches.len() == 1 {
                    let mut args = fluent::FluentArgs::new();
                    let m = wanted_matches.first().unwrap();
                    args.insert("link", m.url().into());
                    let rating =
                        get_message(&bundle, get_rating_bundle_name(&m.rating), None).unwrap();
                    args.insert("rating", rating.into());

                    get_message(bundle, "automatic-single", Some(args)).unwrap()
                } else {
                    let mut buf = String::new();

                    buf.push_str(&get_message(bundle, "automatic-multiple", None).unwrap());
                    buf.push('\n');

                    for result in wanted_matches {
                        let mut args = fluent::FluentArgs::new();
                        args.insert("link", result.url().into());
                        let rating =
                            get_message(&bundle, get_rating_bundle_name(&result.rating), None)
                                .unwrap();
                        args.insert("rating", rating.into());

                        buf.push_str(
                            &get_message(bundle, "automatic-multiple-result", Some(args)).unwrap(),
                        );
                        buf.push('\n');
                    }

                    buf
                }
            })
            .await;

        drop(action);

        let message = SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            disable_web_page_preview: Some(true),
            disable_notification: Some(true),
            text,
            ..Default::default()
        };

        // It's possible the user has deleted the message before a source could
        // be determined so ignore any bad request errors.
        match handler.make_request(&message).await {
            Ok(_)
            | Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(400),
                ..
            })) => Ok(Completed),
            Err(err) => Err(err).context("Unable to send group source message"),
        }
    }
}
