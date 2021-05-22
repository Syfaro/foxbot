use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::*;

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_utils::*;

pub struct ErrorCleanup;

#[async_trait]
impl Handler for ErrorCleanup {
    fn name(&self) -> &'static str {
        "error_cleanup"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        let callback_query = needs_field!(update, callback_query);
        let data = needs_field!(callback_query, data);

        if data != "delete" {
            return Ok(Ignored);
        }

        let message = match &callback_query.message {
            Some(message) => message,
            None => {
                let lang = callback_query.from.language_code.as_deref();
                let message = handler
                    .get_fluent_bundle(lang, |bundle| {
                        get_message(bundle, "error-delete-callback", None).unwrap()
                    })
                    .await;

                let answer = requests::AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    text: Some(message),
                    ..Default::default()
                };

                handler
                    .make_request(&answer)
                    .await
                    .context("Unable to answer callback query for missing message")?;

                return Ok(Completed);
            }
        };

        let message_id = message.message_id;
        let chat_id = message.chat_id();

        let delete = requests::DeleteMessage {
            chat_id,
            message_id,
        };

        let err = match handler.make_request(&delete).await {
            Ok(_resp) => None,
            Err(Error::Telegram(err)) if err.error_code == Some(400) => None,
            Err(err) => Some(err.to_string()),
        };

        let text = match err {
            Some(err) => format!("Error: {}", &err[0..100]),
            None => {
                let lang = callback_query.from.language_code.as_deref();
                handler
                    .get_fluent_bundle(lang, |bundle| {
                        get_message(bundle, "error-deleted", None).unwrap()
                    })
                    .await
            }
        };

        let answer = requests::AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        handler
            .make_request(&answer)
            .await
            .context("Unable to answer callback query after deleting message")?;

        Ok(Completed)
    }
}
