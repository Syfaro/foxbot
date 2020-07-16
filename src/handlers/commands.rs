use async_trait::async_trait;
use std::collections::HashMap;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::models::{GroupConfig, GroupConfigKey, Twitter, TwitterRequest};
use crate::needs_field;
use crate::utils::{
    build_alternate_response, continuous_action, find_best_photo, find_images, get_message,
    match_image, parse_known_bots, sort_results,
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
            if username.to_lowercase() != bot_username.to_lowercase() {
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
            "/grouppreviews" => self.group_nopreviews(&handler, &message).await,
            _ => {
                tracing::info!("unknown command: {}", command.name);
                return Ok(Ignored);
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

        let request_token = egg_mode::auth::request_token(&con_token, "oob").await?;

        let conn = handler.conn.check_out().await?;
        Twitter::set_request(
            &conn,
            user.id,
            TwitterRequest {
                request_key: request_token.key.to_string(),
                request_secret: request_token.secret.to_string(),
            },
        )
        .await?;

        let url = egg_mode::auth::authorize_url(&request_token);

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

        handler.make_request(&send_message).await?;

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

        let action = continuous_action(
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

        drop(action);

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
                    ..Default::default()
                };

                handler.make_request(&video).await?;
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

                handler.make_request(&photo).await?;
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

                handler.make_request(&media_group).await?;
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

            handler.make_request(&send_message).await?;
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

        let conn = handler.conn.check_out().await?;

        let is_admin: Option<bool> =
            GroupConfig::get(&conn, message.chat.id, GroupConfigKey::IsAdmin).await?;
        let is_admin = match is_admin {
            Some(admin) => admin,
            None => {
                let get_chat_member = GetChatMember {
                    user_id: handler.bot_user.id,
                    chat_id: message.chat_id(),
                };
                let bot_member = handler.make_request(&get_chat_member).await?;

                let is_admin = bot_member.status.is_admin();

                GroupConfig::set(
                    &conn,
                    GroupConfigKey::IsAdmin,
                    message.chat.id,
                    false,
                    is_admin,
                )
                .await?;

                is_admin
            }
        };

        let summoning_id = message.message_id;

        let (mut reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            let reply = &**reply_to_message;
            let reply_id = if is_admin {
                reply.message_id
            } else {
                message.message_id
            };

            (reply_id, reply)
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

        if is_admin {
            let delete_message = DeleteMessage {
                chat_id: message.chat_id(),
                message_id: summoning_id,
            };

            if let Err(err) = handler.make_request(&delete_message).await {
                reply_to_id = summoning_id;

                match err {
                    tgbotapi::Error::Telegram(_err) => {
                        tracing::warn!("got error trying to delete summoning message");
                        GroupConfig::delete(&conn, GroupConfigKey::IsAdmin, message.chat.id)
                            .await?;
                    }
                    _ => return Err(err.into()),
                }
            }
        }

        let best_photo = find_best_photo(&photo).unwrap();
        let mut matches =
            match_image(&handler.bot, &handler.conn, &handler.fapi, &best_photo).await?;
        sort_results(
            &handler.conn,
            message.from.as_ref().unwrap().id,
            &mut matches,
        )
        .await?;

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

        let disable_preview =
            GroupConfig::get::<bool>(&conn, message.chat.id, GroupConfigKey::GroupNoPreviews)
                .await?
                .is_some()
                || result.distance.unwrap() > 5;

        let send_message = SendMessage {
            chat_id: message.chat.id.into(),
            text,
            disable_web_page_preview: Some(disable_preview),
            reply_to_message_id: Some(reply_to_id),
            ..Default::default()
        };

        handler
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
                    .image_search(&bytes, fuzzysearch::MatchType::Close)
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

        let mut results: HashMap<Vec<String>, Vec<fuzzysearch::File>> = HashMap::new();

        let matches: Vec<fuzzysearch::File> = matches
            .into_iter()
            .map(|m| fuzzysearch::File {
                artists: Some(
                    m.artists
                        .unwrap_or_else(Vec::new)
                        .iter()
                        .map(|artist| artist.to_lowercase())
                        .collect(),
                ),
                ..m
            })
            .collect();

        for m in matches {
            let v = results
                .entry(m.artists.clone().unwrap_or_else(Vec::new))
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

        let sent = handler.make_request(&send_message).await?;

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
                artist.push(fuzzysearch::File {
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
            .make_request(&edit)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    async fn is_valid_admin_group(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
        bot_needs_admin: bool,
    ) -> failure::Fallible<bool> {
        if !message.chat.chat_type.is_group() {
            handler
                .send_generic_reply(&message, "automatic-enable-not-group")
                .await?;
            return Ok(false);
        }

        let user = message.from.as_ref().unwrap();

        let get_chat_member = GetChatMember {
            chat_id: message.chat_id(),
            user_id: user.id,
        };
        let chat_member = handler.make_request(&get_chat_member).await?;

        if !chat_member.status.is_admin() {
            handler
                .send_generic_reply(&message, "automatic-enable-not-admin")
                .await?;
            return Ok(false);
        }

        if !bot_needs_admin {
            return Ok(true);
        }

        let get_chat_member = GetChatMember {
            user_id: handler.bot_user.id,
            ..get_chat_member
        };
        let bot_member = handler.make_request(&get_chat_member).await?;

        if !bot_member.status.is_admin() {
            handler
                .send_generic_reply(&message, "automatic-enable-bot-not-admin")
                .await?;
            return Ok(false);
        }

        Ok(true)
    }

    async fn enable_group_source(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        if !self.is_valid_admin_group(&handler, &message, true).await? {
            return Ok(());
        }

        let conn = handler.conn.check_out().await?;

        let result: Option<bool> =
            GroupConfig::get(&conn, message.chat.id, GroupConfigKey::GroupAdd).await?;

        if result.is_some() {
            GroupConfig::delete(&conn, GroupConfigKey::GroupAdd, message.chat.id).await?;
            handler
                .send_generic_reply(&message, "automatic-disable")
                .await?;
        } else {
            GroupConfig::set(
                &conn,
                GroupConfigKey::GroupAdd,
                message.chat.id,
                false,
                true,
            )
            .await?;
            handler
                .send_generic_reply(&message, "automatic-enable-success")
                .await?;
        }

        Ok(())
    }

    async fn group_nopreviews(
        &self,
        handler: &crate::MessageHandler,
        message: &Message,
    ) -> failure::Fallible<()> {
        if !self.is_valid_admin_group(&handler, &message, false).await? {
            return Ok(());
        }

        let conn = handler.conn.check_out().await?;

        let result: Option<bool> =
            GroupConfig::get(&conn, message.chat.id, GroupConfigKey::GroupNoPreviews).await?;

        if result.is_some() {
            GroupConfig::delete(&conn, GroupConfigKey::GroupNoPreviews, message.chat.id).await?;
            handler
                .send_generic_reply(&message, "automatic-preview-enable")
                .await?;
        } else {
            GroupConfig::set(
                &conn,
                GroupConfigKey::GroupNoPreviews,
                message.chat.id,
                false,
                false,
            )
            .await?;
            handler
                .send_generic_reply(&message, "automatic-preview-disable")
                .await?;
        }

        Ok(())
    }
}
