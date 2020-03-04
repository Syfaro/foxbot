use async_trait::async_trait;
use sentry::integrations::failure::capture_fail;
use std::collections::HashMap;
use telegram::*;
use tokio01::runtime::current_thread::block_on_all;

use crate::utils::{
    build_alternate_response, continuous_action, download_by_id, find_best_photo, find_images,
    get_message, parse_known_bots, with_user_scope,
};

// TODO: there's a lot of shared code between these commands.

pub struct CommandHandler;

#[async_trait]
impl crate::Handler for CommandHandler {
    fn name(&self) -> &'static str {
        "command"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: Update,
        _command: Option<Command>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let message = match update.message {
            Some(message) => message,
            _ => return Ok(false),
        };

        let command = match message.get_command() {
            Some(command) => command,
            None => return Ok(false),
        };

        let now = std::time::Instant::now();

        if let Some(username) = command.username {
            let bot_username = handler.bot_user.username.as_ref().unwrap();
            if &username != bot_username {
                tracing::debug!("got command for other bot: {}", username);
                return Ok(false);
            }
        }

        tracing::debug!("got command {}", command.name);

        let from = message.from.clone();

        match command.name.as_ref() {
            "/help" | "/start" => handler.handle_welcome(message, &command.name).await,
            "/twitter" => self.authenticate_twitter(&handler, message).await,
            "/mirror" => self.handle_mirror(&handler, message).await,
            "/source" => self.handle_source(&handler, message).await,
            "/alts" => self.handle_alts(&handler, message).await,
            _ => tracing::info!("unknown command: {}", command.name),
        };

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "command")
            .add_tag("command", command.name)
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = handler.influx.query(&point).await {
            tracing::error!("unable to send command to InfluxDB: {:?}", e);
            with_user_scope(from.as_ref(), None, || {
                capture_fail(&e);
            });
        }

        Ok(true)
    }
}

impl CommandHandler {
    async fn authenticate_twitter(&self, handler: &crate::MessageHandler, message: Message) {
        let now = std::time::Instant::now();

        if message.chat.chat_type != ChatType::Private {
            handler
                .send_generic_reply(&message, "twitter-private")
                .await;
            return;
        }

        let user = message.from.clone().unwrap();

        let con_token = egg_mode::KeyPair::new(
            handler.config.twitter_consumer_key.clone(),
            handler.config.twitter_consumer_secret.clone(),
        );

        let request_token = match block_on_all(egg_mode::request_token(&con_token, "oob")) {
            Ok(req) => req,
            Err(e) => {
                tracing::warn!("unable to get request token: {:?}", e);

                handler
                    .report_error(
                        &message,
                        Some(vec![("command", "twitter".to_string())]),
                        || {
                            sentry::integrations::failure::capture_error(&format_err!(
                                "Unable to get request token: {}",
                                e
                            ))
                        },
                    )
                    .await;
                return;
            }
        };

        {
            let mut lock = handler.db.write().await;

            if let Err(e) = lock.set(
                &format!("authenticate:{}", user.id),
                &(request_token.key.clone(), request_token.secret.clone()),
            ) {
                tracing::warn!("unable to save authenticate: {:?}", e);

                handler
                    .report_error(
                        &message,
                        Some(vec![("command", "twitter".to_string())]),
                        || {
                            sentry::integrations::failure::capture_error(&format_err!(
                                "Unable to save to Twitter database: {}",
                                e
                            ))
                        },
                    )
                    .await;
                return;
            }
        }

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

        if let Err(e) = handler.bot.make_request(&send_message).await {
            tracing::warn!("unable to send message: {:?}", e);
            with_user_scope(Some(&user), None, || {
                capture_fail(&e);
            });
        }

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "new")
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = handler.influx.query(&point).await {
            tracing::error!("unable to send command to InfluxDB: {:?}", e);
            with_user_scope(Some(&user), None, || {
                capture_fail(&e);
            });
        }
    }

    async fn handle_mirror(&self, handler: &crate::MessageHandler, message: Message) {
        let from = message.from.clone().unwrap();

        let _action = continuous_action(
            handler.bot.clone(),
            6,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = message.reply_to_message {
            (message.message_id, *reply_to_message)
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
                .await;
            return;
        }

        let mut results: Vec<crate::PostInfo> = Vec::with_capacity(links.len());

        let missing = {
            let mut sites = handler.sites.lock().await;
            let links = links.iter().map(|link| link.as_str()).collect();
            find_images(&from, links, &mut sites, &mut |info| {
                results.extend(info.results);
            })
            .await
        };

        if results.is_empty() {
            handler
                .send_generic_reply(&message, "mirror-no-results")
                .await;
            return;
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

                if let Err(e) = handler.bot.make_request(&video).await {
                    tracing::error!("unable to make request: {:?}", e);
                    handler
                        .report_error(
                            &message,
                            Some(vec![("command", "mirror".to_string())]),
                            || capture_fail(&e),
                        )
                        .await;
                    return;
                }
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

                if let Err(e) = handler.bot.make_request(&photo).await {
                    tracing::error!("unable to make request: {:?}", e);
                    handler
                        .report_error(
                            &message,
                            Some(vec![("command", "mirror".to_string())]),
                            || capture_fail(&e),
                        )
                        .await;
                    return;
                }
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

                if let Err(e) = handler.bot.make_request(&media_group).await {
                    tracing::error!("unable to make request: {:?}", e);
                    handler
                        .report_error(
                            &message,
                            Some(vec![("command", "mirror".to_string())]),
                            || capture_fail(&e),
                        )
                        .await;
                    return;
                }
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

            if let Err(e) = handler.bot.make_request(&send_message).await {
                tracing::error!("unable to make request: {:?}", e);
                with_user_scope(message.from.as_ref(), None, || {
                    capture_fail(&e);
                });
            }
        }
    }

    async fn handle_source(&self, handler: &crate::MessageHandler, message: Message) {
        let _action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = message.reply_to_message {
            (message.message_id, *reply_to_message)
        } else {
            (message.message_id, message)
        };

        let photo = match message.photo.clone() {
            Some(photo) if !photo.is_empty() => photo,
            _ => {
                handler
                    .send_generic_reply(&message, "source-no-photo")
                    .await;
                return;
            }
        };

        let best_photo = find_best_photo(&photo).unwrap();
        let bytes = match download_by_id(&handler.bot, &best_photo.file_id).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!("unable to download file: {:?}", e);
                let tags = Some(vec![("command", "source".to_string())]);
                handler
                    .report_error(&message, tags, || capture_fail(&e))
                    .await;
                return;
            }
        };

        let matches = match handler
            .fapi
            .image_search(&bytes, fautil::MatchType::Close)
            .await
        {
            Ok(matches) => matches.matches,
            Err(e) => {
                tracing::error!("unable to find matches: {:?}", e);
                let tags = Some(vec![("command", "source".to_string())]);
                handler
                    .report_error(&message, tags, || capture_fail(&e))
                    .await;
                return;
            }
        };

        let result = match matches.first() {
            Some(result) => result,
            None => {
                handler
                    .send_generic_reply(&message, "reverse-no-results")
                    .await;
                return;
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
                message.from.clone().unwrap().language_code.as_deref(),
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

        if let Err(e) = handler.bot.make_request(&send_message).await {
            tracing::error!("Unable to make request: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }

    async fn handle_alts(&self, handler: &crate::MessageHandler, message: Message) {
        let _action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = message.reply_to_message {
            (message.message_id, *reply_to_message)
        } else {
            (message.message_id, message)
        };

        let bytes = match message.photo.clone() {
            Some(photo) => {
                let best_photo = find_best_photo(&photo).unwrap();
                match download_by_id(&handler.bot, &best_photo.file_id).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!("unable to download file: {:?}", e);
                        let tags = Some(vec![("command", "source".to_string())]);
                        handler
                            .report_error(&message, tags, || capture_fail(&e))
                            .await;
                        return;
                    }
                }
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
                        .await;
                    return;
                } else if links.len() > 1 {
                    handler
                        .send_generic_reply(&message, "alternate-multiple-photo")
                        .await;
                    return;
                }

                let mut sites = handler.sites.lock().await;
                let links = links.iter().map(|link| link.as_str()).collect();
                let mut link = None;
                find_images(
                    &message.from.clone().unwrap(),
                    links,
                    &mut sites,
                    &mut |info| {
                        link = info.results.into_iter().next();
                    },
                )
                .await;

                match link {
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
                            .await;
                        return;
                    }
                }
            }
        };

        let matches = match handler
            .fapi
            .image_search(&bytes, fautil::MatchType::Force)
            .await
        {
            Ok(matches) => matches,
            Err(e) => {
                tracing::error!("unable to find matches: {:?}", e);
                let tags = Some(vec![("command", "source".to_string())]);
                handler
                    .report_error(&message, tags, || capture_fail(&e))
                    .await;
                return;
            }
        };

        if matches.matches.is_empty() {
            handler
                .send_generic_reply(&message, "reverse-no-results")
                .await;
            return;
        }

        let hash = matches.hash.to_be_bytes();
        let has_multiple_matches = matches.matches.len() > 1;

        let mut results: HashMap<Vec<String>, Vec<fautil::File>> = HashMap::new();

        let matches: Vec<fautil::File> = matches
            .matches
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
                message.from.clone().unwrap().language_code.as_deref(),
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

        let sent = match handler.bot.make_request(&send_message).await {
            Ok(message) => message.message_id,
            Err(e) => {
                tracing::error!("unable to make request: {:?}", e);
                with_user_scope(message.from.as_ref(), None, || {
                    capture_fail(&e);
                });
                return;
            }
        };

        if !has_multiple_matches {
            return;
        }

        let matches = match handler.fapi.lookup_hashes(used_hashes).await {
            Ok(matches) => matches,
            Err(e) => {
                tracing::error!("unable to find matches: {:?}", e);
                let tags = Some(vec![("command", "source".to_string())]);
                handler
                    .report_error(&message, tags, || capture_fail(&e))
                    .await;
                return;
            }
        };

        if matches.is_empty() {
            handler
                .send_generic_reply(&message, "reverse-no-results")
                .await;
            return;
        }

        for m in matches {
            if let Some(artist) = results.get_mut(&m.artists.clone().unwrap()) {
                let bytes = m.hash.unwrap().to_be_bytes();

                artist.push(fautil::File {
                    id: m.id,
                    site_id: m.site_id,
                    distance: Some(hamming::distance_fast(&bytes, &hash).unwrap()),
                    hash: m.hash,
                    url: m.url,
                    filename: m.filename,
                    artists: m.artists.clone(),
                    site_info: None,
                });
            }
        }

        let items = results
            .iter()
            .map(|item| (item.0, item.1))
            .collect::<Vec<_>>();

        let (updated_text, _used_hashes) = handler
            .get_fluent_bundle(
                message.from.clone().unwrap().language_code.as_deref(),
                |bundle| build_alternate_response(&bundle, items),
            )
            .await;

        if text == updated_text {
            return;
        }

        let edit = EditMessageText {
            chat_id: message.chat_id(),
            message_id: Some(sent),
            text: updated_text,
            disable_web_page_preview: Some(true),
            ..Default::default()
        };

        if let Err(e) = handler.bot.make_request(&edit).await {
            tracing::error!("unable to make request: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }
}
