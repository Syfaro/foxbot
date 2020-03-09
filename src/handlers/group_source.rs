use super::Status::*;
use crate::needs_field;
use crate::utils::{find_best_photo, get_message, match_image};
use async_trait::async_trait;
use telegram::*;

pub static ENABLE_KEY: &str = "group_add";
pub static ENABLE_VALUE: &str = "yes";

pub struct GroupSourceHandler;

#[async_trait]
impl super::Handler for GroupSourceHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<super::Status, failure::Error> {
        use quaint::prelude::*;

        let message = needs_field!(update, message);
        let photo_sizes = needs_field!(message, photo);

        let conn = handler.conn.check_out().await?;
        let results = conn
            .select(
                Select::from_table("group_config").so_that(
                    "chat_id"
                        .equals(message.chat.id)
                        .and("name".equals(ENABLE_KEY)),
                ),
            )
            .await?;

        let row = match results.first() {
            Some(first) => first,
            _ => return Ok(Ignored),
        };

        if row["value"].as_str().unwrap() != ENABLE_VALUE {
            return Ok(Ignored);
        }

        let best_photo = find_best_photo(&photo_sizes).unwrap();
        let matches = match_image(&handler.bot, &handler.conn, &handler.fapi, &best_photo).await?;

        let wanted_matches = matches
            .iter()
            .filter(|m| m.distance.unwrap() <= 3)
            .collect::<Vec<_>>();

        if wanted_matches.is_empty() {
            return Ok(Completed);
        }

        let links = super::channel_photo::extract_links(&message, &handler.finder);
        if wanted_matches
            .iter()
            .any(|m| super::channel_photo::link_was_seen(&links, &m.url()))
        {
            return Ok(Completed);
        }

        let lang = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.as_deref());

        let text = handler
            .get_fluent_bundle(lang, |bundle| {
                if wanted_matches.len() == 1 {
                    let mut args = fluent::FluentArgs::new();
                    args.insert("link", wanted_matches.first().unwrap().url().into());

                    get_message(bundle, "automatic-single", Some(args)).unwrap()
                } else {
                    let mut buf = String::new();

                    buf.push_str(&get_message(bundle, "automatic-multiple", None).unwrap());
                    buf.push('\n');

                    for result in wanted_matches {
                        let mut args = fluent::FluentArgs::new();
                        args.insert("link", result.url().into());
                        args.insert("distance", result.distance.unwrap().into());

                        buf.push_str(
                            &get_message(bundle, "automatic-multiple-result", Some(args)).unwrap(),
                        );
                        buf.push('\n');
                    }

                    buf
                }
            })
            .await;

        let message = SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            disable_web_page_preview: Some(true),
            text,
            ..Default::default()
        };

        handler.bot.make_request(&message).await?;

        Ok(Completed)
    }
}
