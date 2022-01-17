use async_trait::async_trait;
use fluent_bundle::FluentArgs;

use crate::{execute::telegram::Context, models, needs_field, utils, Error};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct TwitterHandler;

#[async_trait]
impl Handler for TwitterHandler {
    fn name(&self) -> &'static str {
        "twitter"
    }

    fn add_jobs(&self, worker_environment: &mut super::WorkerEnvironment) {
        worker_environment.register(crate::web::TWITTER_ACCOUNT_ADDED, |cx, job| async move {
            let mut args = job.args().iter();
            let (telegram_id, username) = crate::extract_args!(args, Option<i64>, String);

            let telegram_id = telegram_id.ok_or(Error::Missing)?;

            let mut args = FluentArgs::new();
            args.set("userName", username);

            let bundle = cx.get_fluent_bundle(None).await;

            let text = utils::get_message(&bundle, "twitter-welcome", Some(args));

            let message = tgbotapi::requests::SendMessage {
                chat_id: telegram_id.into(),
                text,
                ..Default::default()
            };

            cx.make_request(&message).await?;

            Ok(())
        });
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error> {
        match command {
            Some(cmd) if cmd.name == "/twitter" => {
                let message = needs_field!(update, message);
                let user = needs_field!(message, from);
                return handle_command(cx, message, user).await.map(|_| Completed);
            }
            Some(_) => return Ok(Ignored),
            _ => (),
        }

        match &update.callback_query {
            Some(tgbotapi::CallbackQuery {
                data: Some(data), ..
            }) if data == "twitter-add" => {
                let callback = needs_field!(update, callback_query);
                let message = needs_field!(callback, message);
                handle_add(cx, callback, message).await.map(|_| Completed)
            }
            Some(tgbotapi::CallbackQuery {
                data: Some(data), ..
            }) if data == "twitter-remove" => {
                let callback = needs_field!(update, callback_query);
                let message = needs_field!(callback, message);
                handle_remove(cx, callback, message)
                    .await
                    .map(|_| Completed)
            }
            _ => Ok(Ignored),
        }
    }
}

async fn handle_command(
    cx: &Context,
    message: &tgbotapi::Message,
    user: &tgbotapi::User,
) -> Result<(), Error> {
    if message.chat.chat_type != tgbotapi::ChatType::Private {
        cx.send_generic_reply(message, "twitter-private").await?;
        return Ok(());
    }

    if let Some(account) = models::TwitterAccount::get(&cx.pool, user).await? {
        let access = egg_mode::Token::Access {
            consumer: cx.config.twitter_keypair.clone(),
            access: egg_mode::KeyPair::new(account.consumer_key, account.consumer_secret),
        };

        if let Ok(twitter_account) = egg_mode::auth::verify_tokens(&access).await {
            let mut args = FluentArgs::new();
            args.set("account", twitter_account.screen_name.clone());

            let bundle = cx.get_fluent_bundle(user.language_code.as_deref()).await;

            let text = utils::get_message(&bundle, "twitter-existing-account", Some(args));

            let (change, remove) = (
                utils::get_message(&bundle, "twitter-change-anyway", None),
                utils::get_message(&bundle, "twitter-remove-account", None),
            );

            let markup = tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(
                tgbotapi::InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![
                        tgbotapi::InlineKeyboardButton {
                            text: change,
                            callback_data: Some("twitter-add".into()),
                            ..Default::default()
                        },
                        tgbotapi::InlineKeyboardButton {
                            text: remove,
                            callback_data: Some("twitter-remove".into()),
                            ..Default::default()
                        },
                    ]],
                },
            );

            let message = tgbotapi::requests::SendMessage {
                chat_id: user.id.into(),
                reply_markup: Some(markup),
                text,
                ..Default::default()
            };

            cx.make_request(&message).await?;

            return Ok(());
        }
    }

    let link = prepare_authorization_link(cx, user).await?;

    let message = tgbotapi::requests::SendMessage {
        chat_id: user.id.into(),
        text: link,
        ..Default::default()
    };

    cx.make_request(&message).await?;

    Ok(())
}

async fn answer_callback(cx: &Context, callback: &tgbotapi::CallbackQuery) -> Result<(), Error> {
    let answer_callback = tgbotapi::requests::AnswerCallbackQuery {
        callback_query_id: callback.id.clone(),
        ..Default::default()
    };

    cx.make_request(&answer_callback).await?;

    Ok(())
}

async fn handle_add(
    cx: &Context,
    callback: &tgbotapi::CallbackQuery,
    message: &tgbotapi::Message,
) -> Result<(), Error> {
    answer_callback(cx, callback).await?;

    let link = prepare_authorization_link(cx, &callback.from).await?;

    let edit_message = tgbotapi::requests::EditMessageText {
        chat_id: message.chat_id(),
        message_id: Some(message.message_id),
        text: link,
        ..Default::default()
    };

    cx.make_request(&edit_message).await?;

    Ok(())
}

async fn handle_remove(
    cx: &Context,
    callback: &tgbotapi::CallbackQuery,
    message: &tgbotapi::Message,
) -> Result<(), Error> {
    answer_callback(cx, callback).await?;

    models::TwitterAccount::delete(&cx.pool, &callback.from).await?;

    let bundle = cx
        .get_fluent_bundle(callback.from.language_code.as_deref())
        .await;

    let text = utils::get_message(&bundle, "twitter-removed-account", None);

    let edit_message = tgbotapi::requests::EditMessageText {
        chat_id: message.chat_id(),
        message_id: Some(message.message_id),
        text,
        ..Default::default()
    };

    cx.make_request(&edit_message).await?;

    Ok(())
}

async fn prepare_authorization_link(cx: &Context, user: &tgbotapi::User) -> Result<String, Error> {
    let con_token = cx.config.twitter_keypair.clone();

    let request_token =
        egg_mode::auth::request_token(&con_token, &cx.config.twitter_callback).await?;

    models::TwitterAuth::set_request(&cx.pool, user, &request_token.key, &request_token.secret)
        .await?;

    let url = egg_mode::auth::authorize_url(&request_token);

    let mut args = FluentArgs::new();
    args.set("link", url);

    let bundle = cx.get_fluent_bundle(user.language_code.as_deref()).await;

    let text = utils::get_message(&bundle, "twitter-callback", Some(args));

    Ok(text)
}
