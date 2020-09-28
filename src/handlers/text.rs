use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::models::{Twitter, TwitterAccount};
use crate::needs_field;
use crate::utils::get_message;

pub struct TextHandler;

#[async_trait]
impl super::Handler for TextHandler {
    fn name(&self) -> &'static str {
        "text"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        let message = needs_field!(update, message);
        let text = needs_field!(message, text);

        let from = message.from.as_ref().unwrap();

        if text.trim().parse::<i32>().is_err() {
            tracing::trace!("got text that wasn't oob, ignoring");
            return Ok(Ignored);
        }

        tracing::trace!(?text, "checking if message was Twitter code");

        let row = match Twitter::get_request(&handler.conn, from.id)
            .await
            .context("unable to query twitter requests")?
        {
            Some(row) => row,
            _ => return Ok(Ignored),
        };

        tracing::trace!(?text, "we had waiting Twitter code");

        let request_token = egg_mode::KeyPair::new(row.request_key, row.request_secret);

        let con_token = egg_mode::KeyPair::new(
            handler.config.twitter_consumer_key.clone(),
            handler.config.twitter_consumer_secret.clone(),
        );

        let token = egg_mode::auth::access_token(con_token, &request_token, text)
            .await
            .context("unable to get twitter access token")?;

        tracing::trace!("got token");

        let access = match token.0 {
            egg_mode::Token::Access { access, .. } => access,
            _ => unimplemented!(),
        };

        tracing::trace!("got access token");

        Twitter::set_account(
            &handler.conn,
            from.id,
            TwitterAccount {
                consumer_key: access.key.to_string(),
                consumer_secret: access.secret.to_string(),
            },
        )
        .await
        .context("unable to set twitter account data")?;

        let mut args = fluent::FluentArgs::new();
        args.insert("userName", fluent::FluentValue::from(token.2));

        let text = handler
            .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "twitter-welcome", Some(args)).unwrap()
            })
            .await;

        let message = SendMessage {
            chat_id: from.id.into(),
            text,
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler
            .make_request(&message)
            .await
            .context("unable to send twitter welcome message")?;

        Ok(Completed)
    }
}
