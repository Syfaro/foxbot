use super::Status::*;
use crate::needs_field;
use async_trait::async_trait;
use telegram::*;
use tokio01::runtime::current_thread::block_on_all;

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
    ) -> Result<super::Status, failure::Error> {
        use quaint::prelude::*;

        let message = needs_field!(update, message);
        let text = needs_field!(message, text);

        let now = std::time::Instant::now();

        let from = message.from.as_ref().unwrap();

        if text.trim().parse::<i32>().is_err() {
            tracing::trace!("got text that wasn't oob, ignoring");
            return Ok(Ignored);
        }

        tracing::trace!("checking if message was Twitter code");

        let conn = handler.conn.check_out().await?;

        let result = conn
            .select(
                Select::from_table("twitter_auth")
                    .column("request_key")
                    .column("request_secret")
                    .so_that("user_id".equals(from.id)),
            )
            .await?;

        let row = match result.first() {
            Some(row) => row,
            _ => return Ok(Completed),
        };

        tracing::trace!("we had waiting Twitter code");

        let request_token = egg_mode::KeyPair::new(
            row["request_key"].to_string().unwrap(),
            row["request_secret"].to_string().unwrap(),
        );

        let con_token = egg_mode::KeyPair::new(
            handler.config.twitter_consumer_key.clone(),
            handler.config.twitter_consumer_secret.clone(),
        );

        let token = block_on_all(egg_mode::access_token(con_token, &request_token, text))?;

        tracing::trace!("got token");

        let access = match token.0 {
            egg_mode::Token::Access { access, .. } => access,
            _ => unimplemented!(),
        };

        tracing::trace!("got access token");

        conn.delete(Delete::from_table("twitter_account").so_that("user_id".equals(from.id)))
            .await?;

        conn.insert(
            Insert::single_into("twitter_account")
                .value("user_id", from.id)
                .value("consumer_key", access.key.to_string())
                .value("consumer_secret", access.secret.to_string())
                .build(),
        )
        .await?;

        conn.delete(Delete::from_table("twitter_auth").so_that("user_id".equals(from.id)))
            .await?;

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

        handler.bot.make_request(&message).await?;

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "added")
            .add_field("duration", now.elapsed().as_millis() as i64);

        let _ = handler.influx.query(&point).await;

        Ok(Completed)
    }
}
