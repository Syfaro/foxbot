use async_trait::async_trait;
use tgbotapi::*;

use super::{
    Handler,
    Status::{self, *},
};
use crate::{execute::telegram::Context, needs_field, utils, Error};

pub struct ErrorCleanup;

#[async_trait]
impl Handler for ErrorCleanup {
    fn name(&self) -> &'static str {
        "error_cleanup"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        let callback_query = needs_field!(update, callback_query);
        let data = needs_field!(callback_query, data);

        if data != "delete" {
            return Ok(Ignored);
        }

        let message = match &callback_query.message {
            Some(message) => message,
            None => {
                let lang = callback_query.from.language_code.as_deref();
                let bundle = cx.get_fluent_bundle(lang).await;

                let message = utils::get_message(&bundle, "error-delete-callback", None);

                let answer = requests::AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    text: Some(message),
                    ..Default::default()
                };

                cx.bot.make_request(&answer).await?;

                return Ok(Completed);
            }
        };

        let message_id = message.message_id;
        let chat_id = message.chat_id();

        let delete = requests::DeleteMessage {
            chat_id,
            message_id,
        };

        let err = match cx.bot.make_request(&delete).await {
            Ok(_resp) => None,
            Err(tgbotapi::Error::Telegram(err)) if err.error_code == Some(400) => None,
            Err(err) => Some(err.to_string()),
        };

        let text = match err {
            Some(err) => format!("Error: {}", &err[0..100]),
            None => {
                let lang = callback_query.from.language_code.as_deref();
                let bundle = cx.get_fluent_bundle(lang).await;

                utils::get_message(&bundle, "error-deleted", None)
            }
        };

        let answer = requests::AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        cx.bot.make_request(&answer).await?;

        Ok(Completed)
    }
}
