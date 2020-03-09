use async_trait::async_trait;
use std::collections::HashMap;
use telegram::*;
use tokio01::runtime::current_thread::block_on_all;

use super::Status::*;
use crate::needs_field;
use crate::utils::{
    build_alternate_response, continuous_action, find_best_photo, find_images, get_message,
    match_image, parse_known_bots,
};

// TODO: there's a lot of shared code between these commands.

pub struct CommandHandler;

#[async_trait]
impl super::Handler for CommandHandler {
    fn name(&self) -> &'static str {
        "command"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<super::Status, failure::Error> {
        let message = needs_field!(update, message);

        let command = match message.get_command() {
            Some(command) => command,
            None => return Ok(Ignored),
        };

        let now = std::time::Instant::now();

        if let Some(username) = command.username {
            let bot_username = handler.bot_user.username.as_ref().unwrap();
            if &username != bot_username {
                tracing::debug!("got command for other bot: {}", username);
                return Ok(Ignored);
            }
        }

        tracing::debug!("got command {}", command.name);

        match command.name.as_ref() {
            "/help" | "/start" => handler.handle_welcome(message, &command.name).await,
            "/twitter" => self.authenticate_twitter(&handler, message).await,
            "/mirror" => self.handle_mirror(&handler, message).await,
            "/source" => self.handle_source(&handler, message).await,
            "/alts" => self.handle_alts(&handler, message).await,
            "/error" => Err(failure::format_err!("a test error message")),
            "/groupsource" => self.enable_group_source(&handler, message).await,
            _ => {
                tracing::info!("unknown command: {}", command.name);
                Ok(())
            }
        }?;

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "command")
            .add_tag("command", command.name)
            .add_field("duration", now.elapsed().as_millis() as i64);

        let _ = handler.influx.query(&point).await;

        Ok(Completed)
    }
}

impl CommandHandler {
    async fn authenticate_twitter(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        use quaint::prelude::*;

        let now = std::time::Instant::now();

        if message.chat.chat_type != ChatType::Private {
            handler
                .send_generic_reply(&message, "twitter-private")
                .await?;
            return Ok(());
        }

        let user = message.from.as_ref().unwrap();

        let con_token = egg_mode::KeyPair::new(
            handler.config.twitter_consumer_key.clone(),
            handler.config.twitter_consumer_secret.clone(),
        );

        let request_token = block_on_all(egg_mode::request_token(&con_token, "oob"))?;

        let conn = handler.conn.check_out().await?;

        conn.delete(Delete::from_table("twitter_auth").so_that("user_id".equals(user.id)))
            .await?;

        conn.insert(
            Insert::single_into("twitter_auth")
                .value("user_id", user.id)
                .value("request_key", request_token.key.to_string())
                .value("request_secret", request_token.secret.to_string())
                .build(),
        )
        .await?;

        drop(conn);

        let url = egg_mode::authorize_url(&request_token);

        let mut args = fluent::FluentArgs::new();
        args.insert("link", fluent::FluentValue::from(url));

        let text = handler
            .get_fluent_bundle(user.language_code.as_deref(), |bundle| {
                get_message(&bundle, "twitter-oob", Some(args)).unwrap()
            })
            .await;

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text,
            reply_markup: Some(ReplyMarkup::ForceReply(ForceReply::selective())),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        handler.bot.make_request(&send_message).await?;

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "new")
            .add_field("duration", now.elapsed().as_millis() as i64);

        let _ = handler.influx.query(&point).await;

        Ok(())
    }

    async fn handle_mirror(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        let from = message.from.as_ref().unwrap();

        let _action = continuous_action(
            handler.bot.clone(),
            6,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            (message.message_id, &**reply_to_message)
        } else {
            (message.message_id, message)
        };

        let links: Vec<_> = if let Some(text) = &message.text {
            handler.finder.links(&text).collect()
        } else {
            vec![]
        };

        if links.is_empty() {
            handler
                .send_generic_reply(&message, "mirror-no-links")
                .await?;
            return Ok(());
        }

        let mut results: Vec<crate::PostInfo> = Vec::with_capacity(links.len());

        let missing = {
            let mut sites = handler.sites.lock().await;
            let links = links.iter().map(|link| link.as_str()).collect();
            find_images(&from, links, &mut sites, &mut |info| {
                results.extend(info.results);
            })
            .await?
        };

        if results.is_empty() {
            handler
                .send_generic_reply(&message, "mirror-no-results")
                .await?;
            return Ok(());
        }

        if results.len() == 1 {
            let result = results.get(0).unwrap();

            if result.file_type == "mp4" {
                let video = SendVideo {
                    chat_id: message.chat_id(),
                    caption: if let Some(source_link) = &result.source_link {
                        Some(source_link.to_owned())
                    } else {
                        None
                    },
                    video: FileType::URL(result.url.clone()),
                    reply_to_message_id: Some(message.message_id),
                };

                handler.bot.make_request(&video).await?;
            } else {
                let photo = SendPhoto {
                    chat_id: message.chat_id(),
                    caption: if let Some(source_link) = &result.source_link {
                        Some(source_link.to_owned())
                    } else {
                        None
                    },
                    photo: FileType::URL(result.url.clone()),
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                handler.bot.make_request(&photo).await?;
            }
        } else {
            for chunk in results.chunks(10) {
                let media = chunk
                    .iter()
                    .map(|result| match result.file_type.as_ref() {
                        "mp4" => InputMedia::Video(InputMediaVideo {
                            media: FileType::URL(result.url.to_owned()),
                            caption: if let Some(source_link) = &result.source_link {
                                Some(source_link.to_owned())
                            } else {
                                None
                            },
                            ..Default::default()
                        }),
                        _ => InputMedia::Photo(InputMediaPhoto {
                            media: FileType::URL(result.url.to_owned()),
                            caption: if let Some(source_link) = &result.source_link {
                                Some(source_link.to_owned())
                            } else {
                                None
                            },
                            ..Default::default()
                        }),
                    })
                    .collect();

                let media_group = SendMediaGroup {
                    chat_id: message.chat_id(),
                    reply_to_message_id: Some(message.message_id),
                    media,
                    ..Default::default()
                };

                handler.bot.make_request(&media_group).await?;
            }
        }

        if !missing.is_empty() {
            let links: Vec<String> = missing.iter().map(|item| format!("· {}", item)).collect();
            let mut args = fluent::FluentArgs::new();
            args.insert("links", fluent::FluentValue::from(links.join("\n")));

            let text = handler
                .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                    get_message(&bundle, "mirror-missing", Some(args)).unwrap()
                })
                .await;

            let send_message = SendMessage {
                chat_id: message.chat_id(),
                reply_to_message_id: Some(reply_to_id),
                text,
                disable_web_page_preview: Some(true),
                ..Default::default()
            };

            handler.bot.make_request(&send_message).await?;
        }

        Ok(())
    }

    async fn handle_source(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        let _action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            (message.message_id, &**reply_to_message)
        } else {
            (message.message_id, message)
        };

        let photo = match &message.photo {
            Some(photo) if !photo.is_empty() => photo,
            _ => {
                handler
                    .send_generic_reply(&message, "source-no-photo")
                    .await?;
                return Ok(());
            }
        };

        let best_photo = find_best_photo(&photo).unwrap();
        let matches = match_image(&handler.bot, &handler.conn, &handler.fapi, &best_photo).await?;

        let result = match matches.first() {
            Some(result) => result,
            None => {
                handler
                    .send_generic_reply(&message, "reverse-no-results")
                    .await?;
                return Ok(());
            }
        };

        let name = if result.distance.unwrap() < 5 {
            "reverse-good-result"
        } else {
            "reverse-bad-result"
        };

        let mut args = fluent::FluentArgs::new();
        args.insert(
            "distance",
            fluent::FluentValue::from(result.distance.unwrap()),
        );
        args.insert("link", fluent::FluentValue::from(result.url()));

        let text = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| get_message(&bundle, name, Some(args)).unwrap(),
            )
            .await;

        let send_message = SendMessage {
            chat_id: message.chat.id.into(),
            text,
            disable_web_page_preview: Some(result.distance.unwrap() > 5),
            reply_to_message_id: Some(reply_to_id),
            ..Default::default()
        };

        handler
            .bot
            .make_request(&send_message)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    async fn handle_alts(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        let _action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message): (i32, &Message) =
            if let Some(reply_to_message) = &message.reply_to_message {
                (message.message_id, &**reply_to_message)
            } else {
                (message.message_id, message)
            };

        let matches = match &message.photo {
            Some(photo) => {
                let best_photo = find_best_photo(&photo).unwrap();
                match_image(&handler.bot, &handler.conn, &handler.fapi, &best_photo).await?
            }
            None => {
                let mut links = vec![];

                if let Some(bot_links) = parse_known_bots(&message) {
                    tracing::trace!("is known bot, adding links");
                    links.extend(bot_links);
                } else if let Some(text) = &message.text {
                    tracing::trace!("message had text, looking at links");
                    links.extend(
                        handler
                            .finder
                            .links(&text)
                            .map(|link| link.as_str().to_string()),
                    );
                }

                if links.is_empty() {
                    handler
                        .send_generic_reply(&message, "source-no-photo")
                        .await?;
                    return Ok(());
                } else if links.len() > 1 {
                    handler
                        .send_generic_reply(&message, "alternate-multiple-photo")
                        .await?;
                    return Ok(());
                }

                let mut sites = handler.sites.lock().await;
                let links = links.iter().map(|link| link.as_str()).collect();
                let mut link = None;
                find_images(
                    &message.from.as_ref().unwrap(),
                    links,
                    &mut sites,
                    &mut |info| {
                        link = info.results.into_iter().next();
                    },
                )
                .await?;

                let bytes = match link {
                    Some(link) => reqwest::get(&link.url)
                        .await
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap()
                        .to_vec(),
                    None => {
                        handler
                            .send_generic_reply(&message, "source-no-photo")
                            .await?;
                        return Ok(());
                    }
                };

                handler
                    .fapi
                    .image_search(&bytes, fautil::MatchType::Close)
                    .await?
                    .matches
            }
        };

        if matches.is_empty() {
            handler
                .send_generic_reply(&message, "reverse-no-results")
                .await?;
            return Ok(());
        }

        let has_multiple_matches = matches.len() > 1;

        let mut results: HashMap<Vec<String>, Vec<fautil::File>> = HashMap::new();

        let matches: Vec<fautil::File> = matches
            .into_iter()
            .map(|m| fautil::File {
                artists: Some(
                    m.artists
                        .unwrap_or_else(|| vec![])
                        .iter()
                        .map(|artist| artist.to_lowercase())
                        .collect(),
                ),
                ..m
            })
            .collect();

        for m in matches {
            let v = results
                .entry(m.artists.clone().unwrap_or_else(|| vec![]))
                .or_default();
            v.push(m);
        }

        let items = results
            .iter()
            .map(|item| (item.0, item.1))
            .collect::<Vec<_>>();

        let (text, used_hashes) = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| build_alternate_response(&bundle, items),
            )
            .await;

        drop(_action);

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: text.clone(),
            disable_web_page_preview: Some(true),
            reply_to_message_id: Some(reply_to_id),
            ..Default::default()
        };

        let sent = handler.bot.make_request(&send_message).await?;

        if !has_multiple_matches {
            return Ok(());
        }

        let matches = handler.fapi.lookup_hashes(used_hashes).await?;

        if matches.is_empty() {
            handler
                .send_generic_reply(&message, "reverse-no-results")
                .await?;
            return Ok(());
        }

        for m in matches {
            if let Some(artist) = results.get_mut(&m.artists.clone().unwrap()) {
                artist.push(fautil::File {
                    id: m.id,
                    site_id: m.site_id,
                    distance: hamming::distance_fast(
                        &m.hash.unwrap().to_be_bytes(),
                        &m.searched_hash.unwrap().to_be_bytes(),
                    )
                    .ok(),
                    hash: m.hash,
                    url: m.url,
                    filename: m.filename,
                    artists: m.artists.clone(),
                    site_info: None,
                    searched_hash: None,
                });
            }
        }

        let items = results
            .iter()
            .map(|item| (item.0, item.1))
            .collect::<Vec<_>>();

        let (updated_text, _used_hashes) = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| build_alternate_response(&bundle, items),
            )
            .await;

        if text == updated_text {
            return Ok(());
        }

        let edit = EditMessageText {
            chat_id: message.chat_id(),
            message_id: Some(sent.message_id),
            text: updated_text,
            disable_web_page_preview: Some(true),
            ..Default::default()
        };

        handler
            .bot
            .make_request(&edit)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    async fn enable_group_source(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        use super::group_source::{ENABLE_KEY, ENABLE_VALUE};
        use quaint::prelude::*;

        if !message.chat.chat_type.is_group() {
            handler
                .send_generic_reply(&message, "automatic-enable-not-group")
                .await?;
            return Ok(());
        }

        let user = message.from.as_ref().unwrap();

        let get_chat_member = GetChatMember {
            chat_id: message.chat_id(),
            user_id: user.id,
        };
        let chat_member = handler.bot.make_request(&get_chat_member).await?;

        if !chat_member.status.is_admin() {
            handler
                .send_generic_reply(&message, "automatic-enable-not-admin")
                .await?;
            return Ok(());
        }

        let get_chat_member = GetChatMember {
            user_id: handler.bot_user.id,
            ..get_chat_member
        };
        let bot_member = handler.bot.make_request(&get_chat_member).await?;

        if !bot_member.status.is_admin() {
            handler
                .send_generic_reply(&message, "automatic-enable-bot-not-admin")
                .await?;
            return Ok(());
        }

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

        if !results.is_empty() {
            conn.delete(
                Delete::from_table("group_config").so_that(
                    "chat_id"
                        .equals(message.chat.id)
                        .and("name".equals(ENABLE_KEY)),
                ),
            )
            .await?;

            handler
                .send_generic_reply(&message, "automatic-disable")
                .await?;

            return Ok(());
        }

        conn.insert(
            Insert::single_into("group_config")
                .value("chat_id", message.chat.id)
                .value("name", ENABLE_KEY)
                .value("value", ENABLE_VALUE)
                .build(),
        )
        .await?;

        handler
            .send_generic_reply(&message, "automatic-enable-success")
            .await?;

        Ok(())
    }
}
