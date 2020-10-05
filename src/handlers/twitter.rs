use super::Status::{self, *};
use crate::models::{Twitter, TwitterAccount};
use crate::utils::get_message;
use crate::{needs_field, MessageHandler};
use async_trait::async_trait;

pub struct TwitterHandler;

impl TwitterHandler {
    async fn handle_command(
        handler: &MessageHandler,
        message: &tgbotapi::Message,
        user: &tgbotapi::User,
    ) -> anyhow::Result<()> {
        if message.chat.chat_type != tgbotapi::ChatType::Private {
            handler
                .send_generic_reply(&message, "twitter-private")
                .await?;
            return Ok(());
        }

        if let Some(account) = Twitter::get_account(&handler.conn, user.id).await? {
            let access = Self::get_access(&handler.config, account);

            if let Ok(twitter_account) = egg_mode::auth::verify_tokens(&access).await {
                let mut args = fluent::FluentArgs::new();
                args.insert("account", twitter_account.screen_name.clone().into());

                let text = handler
                    .get_fluent_bundle(user.language_code.as_deref(), |bundle| {
                        get_message(&bundle, "twitter-existing-account", Some(args)).unwrap()
                    })
                    .await;

                let (change, remove) = handler
                    .get_fluent_bundle(user.language_code.as_deref(), |bundle| {
                        (
                            get_message(&bundle, "twitter-change-anyway", None).unwrap(),
                            get_message(&bundle, "twitter-remove-account", None).unwrap(),
                        )
                    })
                    .await;

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

                handler.make_request(&message).await?;

                return Ok(());
            }
        }

        let link = Self::prepare_authorization_link(&handler, &user).await?;

        let message = tgbotapi::requests::SendMessage {
            chat_id: user.id.into(),
            text: link,
            ..Default::default()
        };

        handler.make_request(&message).await?;

        Ok(())
    }

    async fn verify_account(
        handler: &MessageHandler,
        message: &tgbotapi::Message,
    ) -> anyhow::Result<()> {
        if message.message_id != 0 {
            handler
                .send_generic_reply(&message, "twitter-not-for-you")
                .await?;
            return Ok(());
        }

        let text = message.text.clone().unwrap();
        let mut args = text.split(' ').skip(1);
        let token = args.next().unwrap();
        let verifier = args.next().unwrap();

        let row = match Twitter::get_request(&handler.conn, &token).await? {
            Some(row) => row,
            _ => return Ok(()),
        };

        let request_token = egg_mode::KeyPair::new(row.request_key, row.request_secret);
        let con_token = Self::get_keypair(&handler.config);

        let token = egg_mode::auth::access_token(con_token, &request_token, verifier).await?;

        let access = match token.0 {
            egg_mode::Token::Access { access, .. } => access,
            _ => unreachable!(),
        };

        Twitter::set_account(
            &handler.conn,
            row.user_id,
            crate::models::TwitterAccount {
                consumer_key: access.key.to_string(),
                consumer_secret: access.secret.to_string(),
            },
        )
        .await?;

        let mut args = fluent::FluentArgs::new();
        args.insert("userName", fluent::FluentValue::from(token.2));

        let text = handler
            .get_fluent_bundle(None, |bundle| {
                get_message(&bundle, "twitter-welcome", Some(args)).unwrap()
            })
            .await;

        let message = tgbotapi::requests::SendMessage {
            chat_id: row.user_id.into(),
            text,
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler.make_request(&message).await?;

        Ok(())
    }

    async fn answer_callback(
        handler: &MessageHandler,
        callback: &tgbotapi::CallbackQuery,
    ) -> anyhow::Result<()> {
        let answer_callback = tgbotapi::requests::AnswerCallbackQuery {
            callback_query_id: callback.id.clone(),
            ..Default::default()
        };

        handler.make_request(&answer_callback).await?;

        Ok(())
    }

    async fn handle_add(
        handler: &MessageHandler,
        callback: &tgbotapi::CallbackQuery,
        message: &tgbotapi::Message,
    ) -> anyhow::Result<()> {
        Self::answer_callback(&handler, &callback).await?;

        let link = Self::prepare_authorization_link(&handler, &callback.from).await?;

        let edit_message = tgbotapi::requests::EditMessageText {
            chat_id: message.chat_id(),
            message_id: Some(message.message_id),
            text: link,
            ..Default::default()
        };

        handler.make_request(&edit_message).await?;

        Ok(())
    }

    async fn handle_remove(
        handler: &MessageHandler,
        callback: &tgbotapi::CallbackQuery,
        message: &tgbotapi::Message,
    ) -> anyhow::Result<()> {
        Self::answer_callback(&handler, &callback).await?;

        Twitter::remove_account(&handler.conn, callback.from.id).await?;

        let text = handler
            .get_fluent_bundle(callback.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "twitter-removed-account", None).unwrap()
            })
            .await;

        let edit_message = tgbotapi::requests::EditMessageText {
            chat_id: message.chat_id(),
            message_id: Some(message.message_id),
            text,
            ..Default::default()
        };

        handler.make_request(&edit_message).await?;

        Ok(())
    }

    async fn prepare_authorization_link(
        handler: &MessageHandler,
        user: &tgbotapi::User,
    ) -> anyhow::Result<String> {
        let con_token = Self::get_keypair(&handler.config);

        let request_token =
            egg_mode::auth::request_token(&con_token, &handler.config.twitter_callback).await?;

        Twitter::set_request(
            &handler.conn,
            user.id,
            &request_token.key,
            &request_token.secret,
        )
        .await?;

        let url = egg_mode::auth::authorize_url(&request_token);

        let mut args = fluent::FluentArgs::new();
        args.insert("link", fluent::FluentValue::from(url));

        let text = handler
            .get_fluent_bundle(user.language_code.as_deref(), |bundle| {
                get_message(&bundle, "twitter-callback", Some(args)).unwrap()
            })
            .await;

        Ok(text)
    }

    fn get_keypair(config: &crate::Config) -> egg_mode::KeyPair {
        egg_mode::KeyPair::new(
            config.twitter_consumer_key.clone(),
            config.twitter_consumer_secret.clone(),
        )
    }

    fn get_access(config: &crate::Config, account: TwitterAccount) -> egg_mode::Token {
        egg_mode::Token::Access {
            consumer: Self::get_keypair(&config),
            access: egg_mode::KeyPair::new(account.consumer_key, account.consumer_secret),
        }
    }
}

#[async_trait]
impl super::Handler for TwitterHandler {
    fn name(&self) -> &'static str {
        "twitter"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &tgbotapi::Update,
        command: Option<&tgbotapi::Command>,
    ) -> anyhow::Result<Status> {
        match command {
            Some(cmd) if cmd.name == "/twitter" => {
                let message = needs_field!(update, message);
                let user = needs_field!(message, from);
                return Self::handle_command(&handler, &message, &user)
                    .await
                    .map(|_| Completed);
            }
            Some(cmd) if cmd.name == "/twitterverify" => {
                let message = needs_field!(update, message);
                return Self::verify_account(&handler, &message)
                    .await
                    .map(|_| Completed);
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
                Self::handle_add(&handler, &callback, &message)
                    .await
                    .map(|_| Completed)
            }
            Some(tgbotapi::CallbackQuery {
                data: Some(data), ..
            }) if data == "twitter-remove" => {
                let callback = needs_field!(update, callback_query);
                let message = needs_field!(callback, message);
                Self::handle_remove(&handler, &callback, &message)
                    .await
                    .map(|_| Completed)
            }
            _ => Ok(Ignored),
        }
    }
}