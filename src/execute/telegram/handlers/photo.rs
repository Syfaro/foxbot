use async_trait::async_trait;
use tgbotapi::{
    requests::{ChatAction, SendMessage},
    ChatType, Command, Update,
};

use crate::{execute::telegram::Context, models, needs_field, utils, Error};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct PhotoHandler;

#[async_trait]
impl Handler for PhotoHandler {
    fn name(&self) -> &'static str {
        "photo"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        let message = needs_field!(update, message);
        let photos = needs_field!(message, photo);

        if message.chat.chat_type != ChatType::Private {
            return Ok(Ignored);
        }

        if matches!(message.via_bot, Some(tgbotapi::User { id, .. }) if id == cx.bot_user.id) {
            return Ok(Ignored);
        }

        let action =
            utils::continuous_action(cx.bot.clone(), 12, message.chat_id(), ChatAction::Typing);

        let best_photo = utils::find_best_photo(photos).unwrap();
        let (hash, mut matches) =
            utils::match_image(&cx.bot, &cx.redis, &cx.fuzzysearch, best_photo, Some(3)).await?;

        utils::sort_results(&cx.pool, message.from.as_ref().unwrap(), &mut matches).await?;

        // Typically the response for no sources is handled by the source_reply
        // function, but we need custom handling to allow for subscribing to
        // updates on this hash.

        let bundle = cx
            .get_fluent_bundle(message.from.as_ref().unwrap().language_code.as_deref())
            .await;

        if matches.is_empty() {
            let (text, subscribe) = (
                utils::get_message(&bundle, "reverse-no-results", None).unwrap(),
                utils::get_message(&bundle, "reverse-subscribe", None).unwrap(),
            );

            cx.make_request(&SendMessage {
                chat_id: message.chat_id(),
                text,
                reply_to_message_id: Some(message.message_id),
                reply_markup: Some(tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(
                    tgbotapi::InlineKeyboardMarkup {
                        inline_keyboard: vec![vec![tgbotapi::InlineKeyboardButton {
                            text: subscribe,
                            callback_data: Some(format!("notify-{}", hash)),
                            ..Default::default()
                        }]],
                    },
                )),
                ..Default::default()
            })
            .await?;

            return Ok(Completed);
        }

        let text = utils::source_reply(&matches, &bundle);

        drop(action);

        let disable_preview = models::GroupConfig::get::<_, _, bool>(
            &cx.pool,
            models::GroupConfigKey::GroupNoPreviews,
            &message.chat,
        )
        .await?
        .is_some();

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text,
            disable_web_page_preview: Some(disable_preview),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        cx.make_request(&send_message).await?;

        Ok(Completed)
    }
}
