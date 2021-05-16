use async_trait::async_trait;
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

        let custom = get_faktory_custom();

        let faktory = handler.faktory.clone();
        tokio::task::spawn_blocking(move || {
            let mut faktory = faktory.lock().unwrap();
            let message = serde_json::to_value(hash.to_string()).unwrap();
            let mut job =
                faktory::Job::new("hash_new", vec![message]).on_queue("foxbot_background");
            job.custom = custom;

            faktory.enqueue(job).unwrap();
        });

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

        // TODO: it's possible FuzzySearch sent the hash in the time between
        // when the message was originally sent and the user subscribed. Check
        // if the hash exists now.

        if let Some(tgbotapi::Message {
            message_id,
            chat: tgbotapi::Chat { id, .. },
            ..
        }) = callback_query.message.as_deref()
        {
            let _ = handler
                .bot
                .make_request(&EditMessageReplyMarkup {
                    chat_id: (*id).into(),
                    message_id: Some(*message_id),
                    reply_markup: None,
                    ..Default::default()
                })
                .await;
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
