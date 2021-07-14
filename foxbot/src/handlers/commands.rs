use anyhow::Context;
use async_trait::async_trait;
use std::collections::HashMap;
use tgbotapi::{requests::*, *};

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_models::{ChatAdmin, GroupConfig, GroupConfigKey};
use foxbot_sites::PostInfo;
use foxbot_utils::*;

// TODO: there's a lot of shared code between these commands.

lazy_static::lazy_static! {
    static ref USED_COMMANDS: prometheus::HistogramVec = prometheus::register_histogram_vec!("foxbot_commands_duration_seconds", "Processing duration for each command", &["command"]).unwrap();
}

pub struct CommandHandler;

#[async_trait]
impl Handler for CommandHandler {
    fn name(&self) -> &'static str {
        "command"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        let message = needs_field!(update, message);

        let command = match message.get_command() {
            Some(command) => command,
            None => return Ok(Ignored),
        };

        if let Some(username) = command.username {
            let bot_username = handler.bot_user.username.as_ref().unwrap();
            if username.to_lowercase() != bot_username.to_lowercase() {
                tracing::debug!(?username, "got command for other bot");
                return Ok(Ignored);
            }
        }

        let _hist = USED_COMMANDS
            .get_metric_with_label_values(&[command.name.as_ref()])
            .unwrap()
            .start_timer();
        tracing::debug!(command = ?command.name, "got command");

        match command.name.as_ref() {
            "/help" | "/start" => handler.handle_welcome(message, &command.name).await,
            "/mirror" => self.handle_mirror(handler, message).await,
            "/source" => self.handle_source(handler, message).await,
            "/alts" => self.handle_alts(handler, message).await,
            "/error" => Err(anyhow::anyhow!("a test error message")),
            "/groupsource" => self.enable_group_source(handler, message).await,
            "/grouppreviews" => self.group_nopreviews(handler, message).await,
            "/groupalbums" => self.group_noalbums(handler, message).await,
            _ => {
                tracing::info!(command = ?command.name, "unknown command");
                return Ok(Ignored);
            }
        }?;

        Ok(Completed)
    }
}

impl CommandHandler {
    async fn handle_mirror(
        &self,
        handler: &MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        let from = message.from.as_ref().unwrap();

        let action = continuous_action(
            handler.bot.clone(),
            6,
            message.chat_id(),
            message.from.clone(),
            ChatAction::UploadPhoto,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            (message.message_id, &**reply_to_message)
        } else {
            (message.message_id, message)
        };

        let links = extract_links(message);

        if links.is_empty() {
            drop(action);

            handler
                .send_generic_reply(message, "mirror-no-links")
                .await?;
            return Ok(());
        }

        let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());

        let mut missing = {
            let mut sites = handler.sites.lock().await;
            find_images(from, links, &mut sites, &mut |info| {
                results.extend(info.results);
            })
            .await?
        };

        drop(action);

        if results.is_empty() {
            handler
                .send_generic_reply(message, "mirror-no-results")
                .await?;
            return Ok(());
        }

        // This will only remove duplicate items if they are sequential. This
        // will likely fix the most common issue of having a direct and source
        // link next to each other.
        results.dedup_by(|a, b| a.source_link == b.source_link && a.url == b.url);

        if results.len() == 1 {
            let action = continuous_action(
                handler.bot.clone(),
                6,
                message.chat_id(),
                message.from.clone(),
                ChatAction::UploadPhoto,
            );

            let result = results.get(0).unwrap();

            if result.file_type == "mp4" {
                let video = SendVideo {
                    chat_id: message.chat_id(),
                    caption: result.source_link.clone(),
                    video: FileType::Url(result.url.clone()),
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                drop(action);

                handler.make_request(&video).await?;
            } else if let Ok(file_type) = resize_photo(&result.url, 5_000_000).await {
                let photo = SendPhoto {
                    chat_id: message.chat_id(),
                    caption: result.source_link.clone(),
                    photo: file_type,
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                drop(action);

                handler.make_request(&photo).await?;
            } else {
                missing.push(result.source_link.as_deref().unwrap_or(&result.url));
            }
        } else {
            for chunk in results.chunks(10) {
                let action = continuous_action(
                    handler.bot.clone(),
                    6,
                    message.chat_id(),
                    message.from.clone(),
                    ChatAction::UploadPhoto,
                );

                let mut media = Vec::with_capacity(chunk.len());

                for result in chunk {
                    let input = match result.file_type.as_ref() {
                        "mp4" => InputMedia::Video(InputMediaVideo {
                            media: FileType::Url(result.url.to_owned()),
                            caption: result.source_link.clone(),
                            ..Default::default()
                        }),
                        _ => {
                            if let Ok(file_type) = resize_photo(&result.url, 5_000_000).await {
                                InputMedia::Photo(InputMediaPhoto {
                                    media: file_type,
                                    caption: result.source_link.clone(),
                                    ..Default::default()
                                })
                            } else {
                                missing.push(result.source_link.as_deref().unwrap_or(&result.url));
                                continue;
                            }
                        }
                    };

                    media.push(input);
                }

                let media_group = SendMediaGroup {
                    chat_id: message.chat_id(),
                    reply_to_message_id: Some(message.message_id),
                    media,
                    ..Default::default()
                };

                handler.make_request(&media_group).await?;

                drop(action);
            }
        }

        if !missing.is_empty() {
            let links: Vec<String> = missing.iter().map(|item| format!("· {}", item)).collect();
            let mut args = fluent::FluentArgs::new();
            args.insert("links", fluent::FluentValue::from(links.join("\n")));

            let text = handler
                .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                    get_message(bundle, "mirror-missing", Some(args)).unwrap()
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
        handler: &MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        let action = continuous_action(
            handler.bot.clone(),
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
        );

        let can_delete = can_delete_in_chat(
            &handler.bot,
            &handler.conn,
            &message.chat,
            &handler.bot_user,
            false,
        )
        .await?;

        let summoning_id = message.message_id;

        let (mut reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            let reply = &**reply_to_message;
            let reply_id = if can_delete {
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
                drop(action);

                handler
                    .send_generic_reply(message, "source-no-photo")
                    .await?;
                return Ok(());
            }
        };

        if can_delete {
            let delete_message = DeleteMessage {
                chat_id: message.chat_id(),
                message_id: summoning_id,
            };

            if let Err(err) = handler.make_request(&delete_message).await {
                reply_to_id = summoning_id;

                match err {
                    tgbotapi::Error::Telegram(_err) => {
                        tracing::warn!("got error trying to delete summoning message");
                        // Recheck if we have delete permissions, ignoring cache
                        can_delete_in_chat(
                            &handler.bot,
                            &handler.conn,
                            &message.chat,
                            &handler.bot_user,
                            true,
                        )
                        .await?;
                    }
                    _ => return Err(err.into()),
                }
            }
        }

        let best_photo = find_best_photo(photo).unwrap();
        let mut matches = match_image(
            &handler.bot,
            &handler.redis,
            &handler.fapi,
            best_photo,
            Some(3),
        )
        .await?
        .1;
        sort_results(&handler.conn, message.from.as_ref().unwrap(), &mut matches).await?;

        let text = handler
            .get_fluent_bundle(
                message.from.as_ref().unwrap().language_code.as_deref(),
                |bundle| source_reply(&matches, bundle),
            )
            .await;

        let disable_preview = GroupConfig::get::<bool, _>(
            &handler.conn,
            &message.chat,
            GroupConfigKey::GroupNoPreviews,
        )
        .await?
        .is_some();

        drop(action);

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

    async fn handle_alts(&self, handler: &MessageHandler, message: &Message) -> anyhow::Result<()> {
        let action = continuous_action(
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

        let (searched_hash, matches) = if let Some(sizes) = &message.photo {
            let best_photo = find_best_photo(sizes).unwrap();

            match_image(
                &handler.bot,
                &handler.redis,
                &handler.fapi,
                best_photo,
                Some(10),
            )
            .await?
        } else {
            let from = message
                .from
                .as_ref()
                .context("Message was not sent from a user")?;

            let links = extract_links(message);

            let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());
            let missing = {
                let mut sites = handler.sites.lock().await;
                find_images(from, links, &mut sites, &mut |info| {
                    results.extend(info.results);
                })
                .await?
            };

            if results.len() + missing.len() > 1 {
                drop(action);

                handler
                    .send_generic_reply(message, "alternate-multiple-photo")
                    .await?;
                return Ok(());
            } else if !missing.is_empty() {
                drop(action);

                handler
                    .send_generic_reply(message, "alternate-unknown-link")
                    .await?;
                return Ok(());
            } else if let Some(result) = results.first() {
                let bytes = CheckFileSize::new(&result.url, 20_000_000)
                    .into_bytes()
                    .await?;
                let hash =
                    tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&bytes)).await??;

                (
                    hash,
                    lookup_single_hash(&handler.fapi, hash, Some(10)).await?,
                )
            } else {
                drop(action);

                handler
                    .send_generic_reply(message, "source-no-photo")
                    .await?;
                return Ok(());
            }
        };

        if matches.is_empty() {
            drop(action);

            handler
                .send_generic_reply(message, "reverse-no-results")
                .await?;
            return Ok(());
        }

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
                |bundle| build_alternate_response(bundle, items),
            )
            .await;

        drop(action);

        if used_hashes.is_empty() {
            handler
                .send_generic_reply(message, "reverse-no-results")
                .await?;
            return Ok(());
        }

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: text.clone(),
            disable_web_page_preview: Some(true),
            reply_to_message_id: Some(reply_to_id),
            ..Default::default()
        };

        let sent = handler.make_request(&send_message).await?;

        let matches = handler.fapi.lookup_hashes(&used_hashes, Some(10)).await?;

        if matches.is_empty() {
            handler
                .send_generic_reply(message, "reverse-no-results")
                .await?;
            return Ok(());
        }

        for m in matches {
            if let Some(artist) = results.get_mut(&m.artists.clone().unwrap()) {
                artist.push(fuzzysearch::File {
                    distance: hamming::distance_fast(
                        &m.hash.unwrap().to_be_bytes(),
                        &searched_hash.to_be_bytes(),
                    )
                    .ok(),
                    ..m
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
                |bundle| build_alternate_response(bundle, items),
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
        handler: &MessageHandler,
        message: &Message,
        bot_needs_admin: bool,
    ) -> anyhow::Result<bool> {
        use tgbotapi::ChatMemberStatus::*;

        if !message.chat.chat_type.is_group() {
            handler
                .send_generic_reply(message, "automatic-enable-not-group")
                .await?;
            return Ok(false);
        }

        let user = message.from.as_ref().unwrap();

        // As of the Bot API 5.1, Telegram can now proactively send updates
        // about user information. There is another handler that listens for
        // these changes and saves them to the database. When possible we should
        // use these saved values instead of making more requests.

        let user_is_admin = match ChatAdmin::is_admin(&handler.conn, user, &message.chat).await? {
            Some(is_admin) => is_admin,
            _ => {
                let get_chat_member = GetChatMember {
                    chat_id: message.chat_id(),
                    user_id: user.id,
                };
                let chat_member = handler.make_request(&get_chat_member).await?;

                matches!(chat_member.status, Administrator | Creator)
            }
        };

        if !user_is_admin {
            handler
                .send_generic_reply(message, "automatic-enable-not-admin")
                .await?;
            return Ok(false);
        }

        if !bot_needs_admin {
            return Ok(true);
        }

        let bot_is_admin =
            match ChatAdmin::is_admin(&handler.conn, &handler.bot_user, &message.chat).await? {
                Some(is_admin) => is_admin,
                _ => {
                    let get_chat_member = GetChatMember {
                        chat_id: message.chat_id(),
                        user_id: handler.bot_user.id,
                    };
                    let bot_member = handler.make_request(&get_chat_member).await?;

                    // Already fetching it, should save it for trying to delete summoning
                    // messages.
                    GroupConfig::set(
                        &handler.conn,
                        GroupConfigKey::HasDeletePermission,
                        &message.chat,
                        bot_member.can_delete_messages.unwrap_or(false),
                    )
                    .await?;

                    matches!(bot_member.status, Administrator | Creator)
                }
            };

        if !bot_is_admin {
            handler
                .send_generic_reply(message, "automatic-enable-bot-not-admin")
                .await?;
            return Ok(false);
        }

        Ok(true)
    }

    async fn enable_group_source(
        &self,
        handler: &MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        if !self.is_valid_admin_group(handler, message, true).await? {
            return Ok(());
        }

        let result = GroupConfig::get(&handler.conn, &message.chat, GroupConfigKey::GroupAdd)
            .await?
            .unwrap_or(false);

        GroupConfig::set(
            &handler.conn,
            GroupConfigKey::GroupAdd,
            &message.chat,
            !result,
        )
        .await?;

        let name = if !result {
            "automatic-enable-success"
        } else {
            "automatic-disable"
        };

        handler.send_generic_reply(message, name).await?;

        Ok(())
    }

    async fn group_nopreviews(
        &self,
        handler: &MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        if !self.is_valid_admin_group(handler, message, false).await? {
            return Ok(());
        }

        let result = GroupConfig::get(
            &handler.conn,
            &message.chat,
            GroupConfigKey::GroupNoPreviews,
        )
        .await?
        .unwrap_or(true);

        GroupConfig::set(
            &handler.conn,
            GroupConfigKey::GroupNoPreviews,
            &message.chat,
            !result,
        )
        .await?;

        let name = if !result {
            "automatic-preview-enable"
        } else {
            "automatic-preview-disable"
        };

        handler.send_generic_reply(message, name).await?;

        Ok(())
    }

    async fn group_noalbums(
        &self,
        handler: &MessageHandler,
        message: &Message,
    ) -> anyhow::Result<()> {
        if !self.is_valid_admin_group(handler, message, false).await? {
            return Ok(());
        }

        let result = GroupConfig::get(&handler.conn, &message.chat, GroupConfigKey::GroupNoAlbums)
            .await?
            .unwrap_or(false);

        GroupConfig::set(
            &handler.conn,
            GroupConfigKey::GroupNoAlbums,
            &message.chat,
            !result,
        )
        .await?;

        let name = if result {
            "automatic-album-enable"
        } else {
            "automatic-album-disable"
        };

        handler.send_generic_reply(message, name).await?;

        Ok(())
    }
}
