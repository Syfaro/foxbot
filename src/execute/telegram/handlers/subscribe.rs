use async_trait::async_trait;
use tgbotapi::requests::{AnswerCallbackQuery, EditMessageReplyMarkup};

use crate::{execute::telegram::Context, models, utils, Error};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct SubscribeHandler;

#[async_trait]
impl Handler for SubscribeHandler {
    fn name(&self) -> &'static str {
        "subscribe"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error> {
        if let Some(callback_query) = &update.callback_query {
            if !callback_query
                .data
                .as_deref()
                .unwrap_or_default()
                .starts_with("notify-")
            {
                return Ok(Ignored);
            }

            self.subscribe(cx, callback_query).await?;
            return Ok(Completed);
        }

        Ok(Ignored)
    }
}

impl SubscribeHandler {
    async fn subscribe(
        &self,
        cx: &Context,
        callback_query: &tgbotapi::CallbackQuery,
    ) -> Result<(), Error> {
        let data = callback_query.data.as_deref().unwrap();
        let hash = data.strip_prefix("notify-").unwrap();
        let hash: i64 = hash
            .parse()
            .map_err(|_err| Error::bot("hash could not be decoded"))?;

        tracing::trace!(hash, "attempting to add subscription for hash");

        let bundle = cx
            .get_fluent_bundle(callback_query.from.language_code.as_deref())
            .await;

        let (message_id, photo_id) = match callback_query.message.as_deref() {
            Some(tgbotapi::Message {
                reply_to_message: Some(message),
                ..
            }) => (
                Some(message.message_id),
                message.photo.as_ref().and_then(|photo| {
                    utils::find_best_photo(photo)
                        .iter()
                        .next()
                        .map(|photo| photo.file_id.clone())
                }),
            ),
            Some(message) => (Some(message.message_id), None),
            None => (None, None),
        };

        if let Err(err) = models::Subscription::add(
            &cx.pool,
            &callback_query.from,
            hash,
            message_id,
            photo_id.as_deref(),
        )
        .await
        {
            tracing::error!("unable to add hash subscription: {:?}", err);

            let text = utils::get_message(&bundle, "subscribe-error", None);

            cx.bot
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
            let _ = cx
                .bot
                .make_request(&EditMessageReplyMarkup {
                    chat_id: (*id).into(),
                    message_id: Some(*message_id),
                    reply_markup: None,
                    ..Default::default()
                })
                .await;
        }

        let text = utils::get_message(&bundle, "subscribe-success", None);

        cx.bot
            .make_request(&AnswerCallbackQuery {
                callback_query_id: callback_query.id.clone(),
                text: Some(text),
                ..Default::default()
            })
            .await?;

        Ok(())
    }
}
