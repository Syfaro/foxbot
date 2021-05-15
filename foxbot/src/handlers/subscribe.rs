use async_trait::async_trait;
use fluent::fluent_args;
use tgbotapi::requests::*;

use super::{
    Handler,
    Status::{self, *},
};
use crate::{MessageHandler, ServiceData};
use foxbot_models::Subscriptions;
use foxbot_utils::*;

lazy_static::lazy_static! {
    static ref SUBSCRIBE_DURATION: prometheus::Histogram = prometheus::register_histogram!("foxbot_subscribe_handling_seconds", "Time to process subscriptions").unwrap();
}

pub struct SubscribeHandler;

#[async_trait]
impl Handler for SubscribeHandler {
    fn name(&self) -> &'static str {
        "subscribe"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> anyhow::Result<Status> {
        if let Some(callback_query) = &update.callback_query {
            if !callback_query
                .data
                .as_deref()
                .unwrap_or_default()
                .starts_with("notify-")
            {
                return Ok(Ignored);
            }

            self.subscribe(&handler, &callback_query).await?;
            return Ok(Completed);
        }

        Ok(Ignored)
    }

    async fn handle_service(
        &self,
        handler: &MessageHandler,
        service: &ServiceData,
    ) -> anyhow::Result<()> {
        let hash = match service {
            ServiceData::NewHash { hash } => *hash,
            _ => return Ok(()),
        };

        let hist = SUBSCRIBE_DURATION.start_timer();
        let subscriptions = Subscriptions::search_subscriptions(&handler.conn, hash).await?;
        if subscriptions.is_empty() {
            hist.stop_and_record();
            tracing::trace!("got hash with no subscriptions");
            return Ok(());
        }
        hist.stop_and_record();

        tracing::debug!("found hash with subscriptions, loading full information");

        let matches = lookup_single_hash(&handler.fapi, hash, Some(3)).await?;
        if matches.is_empty() {
            tracing::warn!("got hash notification but found no matches");
            return Ok(());
        }

        let text = if matches.len() == 1 {
            let file = matches.first().unwrap();

            let args = fluent_args![
                "link" => file.url()
            ];

            handler
                .get_fluent_bundle(None, |bundle| {
                    get_message(&bundle, "subscribe-found-single", Some(args)).unwrap()
                })
                .await
        } else {
            handler
                .get_fluent_bundle(None, |bundle| {
                    let mut buf = String::new();

                    buf.push_str(&get_message(bundle, "subscribe-found-multiple", None).unwrap());
                    buf.push('\n');

                    for result in matches {
                        let args = fluent_args![
                            "link" => result.url()
                        ];

                        let message =
                            get_message(bundle, "subscribe-found-multiple-item", Some(args))
                                .unwrap();

                        buf.push_str(&message);
                        buf.push('\n');
                    }

                    buf
                })
                .await
        };

        for sub in subscriptions {
            let mut was_sent = false;

            if let Some(photo_id) = sub.photo_id {
                let send_photo = SendPhoto {
                    photo: tgbotapi::FileType::FileID(photo_id),
                    chat_id: sub.user_id.into(),
                    reply_to_message_id: sub.message_id,
                    caption: Some(text.clone()),
                    ..Default::default()
                };

                if handler.bot.make_request(&send_photo).await.is_ok() {
                    was_sent = true;
                }
            }

            if !was_sent {
                let send_message = SendMessage {
                    chat_id: sub.user_id.into(),
                    reply_to_message_id: sub.message_id,
                    text: text.clone(),
                    ..Default::default()
                };
                handler.bot.make_request(&send_message).await?;
            }

            Subscriptions::remove_subscription(&handler.conn, sub.user_id, sub.hash).await?;
        }

        Ok(())
    }
}

impl SubscribeHandler {
    async fn subscribe(
        &self,
        handler: &MessageHandler,
        callback_query: &tgbotapi::CallbackQuery,
    ) -> anyhow::Result<()> {
        let data = callback_query.data.as_deref().unwrap();
        let hash = data.strip_prefix("notify-").unwrap();
        let hash: i64 = hash.parse()?;

        tracing::trace!(hash, "attempting to add subscription for hash");

        let (message_id, photo_id) = match callback_query.message.as_deref() {
            Some(tgbotapi::Message {
                reply_to_message: Some(message),
                ..
            }) => (
                Some(message.message_id),
                message.photo.as_ref().and_then(|photo| {
                    find_best_photo(&photo)
                        .iter()
                        .next()
                        .map(|photo| photo.file_id.clone())
                }),
            ),
            Some(message) => (Some(message.message_id), None),
            None => (None, None),
        };

        if let Err(err) = Subscriptions::add_subscription(
            &handler.conn,
            callback_query.from.id,
            hash,
            message_id,
            photo_id.as_deref(),
        )
        .await
        {
            tracing::error!("unable to add hash subscription: {:?}", err);
            sentry::integrations::anyhow::capture_anyhow(&err);

            let text = handler
                .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                    get_message(&bundle, "subscribe-error", None).unwrap()
                })
                .await;

            handler
                .bot
                .make_request(&AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    show_alert: Some(true),
                    text: Some(text),
                    ..Default::default()
                })
                .await?;

            return Err(err);
        }

        let text = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "subscribe-success", None).unwrap()
            })
            .await;

        handler
            .bot
            .make_request(&AnswerCallbackQuery {
                callback_query_id: callback_query.id.clone(),
                text: Some(text),
                ..Default::default()
            })
            .await?;

        Ok(())
    }
}
