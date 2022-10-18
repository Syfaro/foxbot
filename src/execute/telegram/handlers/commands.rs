use std::collections::HashMap;
use std::fmt::Write;

use async_trait::async_trait;
use fluent_bundle::FluentArgs;
use futures_retry::FutureRetry;
use prometheus::{register_histogram_vec, HistogramVec};
use tgbotapi::requests::*;

use crate::{
    execute::telegram::Context,
    models, needs_field,
    sites::PostInfo,
    utils::{self, get_message, CheckFileSize},
    Error, Features,
};

use super::{
    Handler,
    Status::{self, *},
};

// TODO: there's a lot of shared code between these commands.

lazy_static::lazy_static! {
    static ref USED_COMMANDS: HistogramVec = register_histogram_vec!("foxbot_command_duration_seconds", "Processing duration for each command", &["command"]).unwrap();
}

pub struct CommandHandler;

#[async_trait]
impl Handler for CommandHandler {
    fn name(&self) -> &'static str {
        "command"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error> {
        let message = needs_field!(update, message);

        let command = match message.get_command() {
            Some(command) => command,
            None => return Ok(Ignored),
        };

        if let Some(username) = command.username {
            let bot_username = cx.bot_user.username.as_ref().unwrap();
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
            "/help" | "/start" => cx.handle_welcome(message, &command.name).await,
            "/mirror" => self.handle_mirror(cx, message).await,
            "/source" => self.handle_source(cx, message).await,
            "/alts" => self.handle_alts(cx, message).await,
            "/groupsource" => self.enable_group_source(cx, message).await,
            "/grouppreviews" => self.group_nopreviews(cx, message).await,
            "/groupalbums" => self.group_noalbums(cx, message).await,
            "/manage" => self.manage(cx, message).await,
            "/trace" => self.trace(cx, message).await,
            "/feedback" => {
                let bundle = cx
                    .get_fluent_bundle(
                        message
                            .from
                            .as_ref()
                            .and_then(|user| user.language_code.as_deref()),
                    )
                    .await;

                let text = get_message(&bundle, "feedback-message", None);
                let button_text = get_message(&bundle, "feedback-button", None);

                cx.bot
                    .make_request(&tgbotapi::requests::SendMessage {
                        chat_id: message.chat_id(),
                        text,
                        reply_markup: Some(tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(
                            tgbotapi::InlineKeyboardMarkup {
                                inline_keyboard: vec![vec![tgbotapi::InlineKeyboardButton {
                                    text: button_text,
                                    login_url: Some(tgbotapi::LoginUrl {
                                        url: format!(
                                            "{}/feedback/login",
                                            cx.config.public_endpoint
                                        ),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                }]],
                            },
                        )),
                        ..Default::default()
                    })
                    .await?;

                return Ok(Completed);
            }
            _ => {
                tracing::info!(command = ?command.name, "unknown command");
                return Ok(Ignored);
            }
        }?;

        Ok(Completed)
    }
}

impl CommandHandler {
    async fn handle_mirror(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let from = message
            .from
            .as_ref()
            .ok_or_else(|| Error::missing("user"))?;

        let action = utils::continuous_action(
            cx.bot.clone(),
            6,
            message.chat_id(),
            ChatAction::UploadPhoto,
        );

        let (reply_to_id, message) = if let Some(reply_to_message) = &message.reply_to_message {
            (message.message_id, &**reply_to_message)
        } else {
            (message.message_id, message)
        };

        let links = utils::extract_links(message);

        if links.is_empty() {
            drop(action);

            cx.send_generic_reply(message, "mirror-no-links").await?;
            return Ok(());
        }

        let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());

        let mut missing = {
            utils::find_images(from, links, &cx.sites, &cx.redis, &mut |info| {
                results.extend(info.results);
            })
            .await?
        };

        drop(action);

        if results.is_empty() {
            cx.send_generic_reply(message, "mirror-no-results").await?;
            return Ok(());
        }

        // This will only remove duplicate items if they are sequential. This
        // will likely fix the most common issue of having a direct and source
        // link next to each other.
        results.dedup_by(|a, b| a.source_link == b.source_link && a.url == b.url);

        if results.len() == 1 {
            let action = utils::continuous_action(
                cx.bot.clone(),
                6,
                message.chat_id(),
                ChatAction::UploadPhoto,
            );

            let result = results.first().unwrap();

            if result.file_type == "mp4" {
                let video = SendVideo {
                    chat_id: message.chat_id(),
                    caption: result.source_link.clone(),
                    video: tgbotapi::FileType::Url(result.url.clone()),
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                drop(action);

                cx.bot.make_request(&video).await?;
            } else if let Ok(file_type) = utils::resize_photo(&result.url, 5_000_000).await {
                let photo = SendPhoto {
                    chat_id: message.chat_id(),
                    caption: result.source_link.clone(),
                    photo: file_type,
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                drop(action);

                cx.bot.make_request(&photo).await?;
            } else {
                missing.push(result.source_link.as_deref().unwrap_or(&result.url));
            }
        } else {
            for chunk in results.chunks(10) {
                let action = utils::continuous_action(
                    cx.bot.clone(),
                    6,
                    message.chat_id(),
                    ChatAction::UploadPhoto,
                );

                let mut media = Vec::with_capacity(chunk.len());

                for result in chunk {
                    let input = match result.file_type.as_ref() {
                        "mp4" => InputMedia::Video(InputMediaVideo {
                            media: tgbotapi::FileType::Url(result.url.to_owned()),
                            caption: result.source_link.clone(),
                            ..Default::default()
                        }),
                        _ => {
                            if let Ok(file_type) = utils::resize_photo(&result.url, 5_000_000).await
                            {
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

                cx.bot.make_request(&media_group).await?;

                drop(action);
            }
        }

        if !missing.is_empty() {
            let links: Vec<String> = missing.iter().map(|item| format!("· {}", item)).collect();
            let mut args = FluentArgs::new();
            args.set("links", links.join("\n"));

            let bundle = cx.get_fluent_bundle(from.language_code.as_deref()).await;

            let text = utils::get_message(&bundle, "mirror-missing", Some(args));

            let send_message = SendMessage {
                chat_id: message.chat_id(),
                reply_to_message_id: Some(reply_to_id),
                text,
                disable_web_page_preview: Some(true),
                ..Default::default()
            };

            cx.bot.make_request(&send_message).await?;
        }

        Ok(())
    }

    async fn handle_source(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let action =
            utils::continuous_action(cx.bot.clone(), 12, message.chat_id(), ChatAction::Typing);

        let can_delete =
            utils::can_delete_in_chat(&cx.bot, &cx.pool, &message.chat, &cx.bot_user, false)
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

                cx.send_generic_reply(message, "source-no-photo").await?;
                return Ok(());
            }
        };

        if can_delete {
            let delete_message = DeleteMessage {
                chat_id: message.chat_id(),
                message_id: summoning_id,
            };

            if let Err(err) = cx.bot.make_request(&delete_message).await {
                reply_to_id = summoning_id;

                match err {
                    tgbotapi::Error::Telegram(_err) => {
                        tracing::warn!("got error trying to delete summoning message");
                        // Recheck if we have delete permissions, ignoring cache
                        utils::can_delete_in_chat(
                            &cx.bot,
                            &cx.pool,
                            &message.chat,
                            &cx.bot_user,
                            true,
                        )
                        .await?;
                    }
                    _ => return Err(err.into()),
                }
            }
        }

        let allow_nsfw = models::GroupConfig::get(
            &cx.pool,
            models::GroupConfigKey::Nsfw,
            models::Chat::Telegram(message.chat.id),
        )
        .await?
        .unwrap_or(true);

        let best_photo = utils::find_best_photo(photo).unwrap();
        let mut matches = utils::match_image(
            &cx.bot,
            &cx.redis,
            &cx.fuzzysearch,
            best_photo,
            Some(3),
            allow_nsfw,
        )
        .await?
        .1;
        utils::sort_results(&cx.pool, message.from.as_ref().unwrap(), &mut matches).await?;

        let bundle = cx
            .get_fluent_bundle(message.from.as_ref().unwrap().language_code.as_deref())
            .await;

        let text = utils::source_reply(&matches, &bundle);

        let disable_preview = models::GroupConfig::get::<_, _, bool>(
            &cx.pool,
            models::GroupConfigKey::GroupNoPreviews,
            &message.chat,
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

        cx.bot
            .make_request(&send_message)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    async fn handle_alts(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let action =
            utils::continuous_action(cx.bot.clone(), 12, message.chat_id(), ChatAction::Typing);

        let (reply_to_id, message): (i32, &tgbotapi::Message) =
            if let Some(reply_to_message) = &message.reply_to_message {
                (message.message_id, &**reply_to_message)
            } else {
                (message.message_id, message)
            };

        let allow_nsfw = models::GroupConfig::get(
            &cx.pool,
            models::GroupConfigKey::Nsfw,
            models::Chat::Telegram(message.chat.id),
        )
        .await?
        .unwrap_or(true);

        let (searched_hash, matches) = if let Some(sizes) = &message.photo {
            let best_photo = utils::find_best_photo(sizes).unwrap();

            utils::match_image(
                &cx.bot,
                &cx.redis,
                &cx.fuzzysearch,
                best_photo,
                Some(10),
                allow_nsfw,
            )
            .await?
        } else {
            let from = message
                .from
                .as_ref()
                .ok_or_else(|| Error::missing("user"))?;

            let links = utils::extract_links(message);

            let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());
            let missing = {
                utils::find_images(from, links, &cx.sites, &cx.redis, &mut |info| {
                    results.extend(info.results);
                })
                .await?
            };

            if results.len() + missing.len() > 1 {
                drop(action);

                cx.send_generic_reply(message, "alternate-multiple-photo")
                    .await?;
                return Ok(());
            } else if !missing.is_empty() {
                drop(action);

                cx.send_generic_reply(message, "alternate-unknown-link")
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
                    utils::lookup_single_hash(&cx.fuzzysearch, hash, Some(10), allow_nsfw).await?,
                )
            } else {
                drop(action);

                cx.send_generic_reply(message, "source-no-photo").await?;
                return Ok(());
            }
        };

        if matches.is_empty() {
            drop(action);

            cx.send_generic_reply(message, "reverse-no-results").await?;
            return Ok(());
        }

        let mut results: HashMap<Vec<String>, Vec<fuzzysearch::File>> = HashMap::new();

        let matches: Vec<fuzzysearch::File> = matches
            .into_iter()
            .map(|m| fuzzysearch::File {
                artists: Some(
                    m.artists
                        .unwrap_or_default()
                        .iter()
                        .map(|artist| artist.to_lowercase())
                        .collect(),
                ),
                ..m
            })
            .collect();

        for m in matches {
            let v = results
                .entry(m.artists.clone().unwrap_or_default())
                .or_default();
            v.push(m);
        }

        let items = results
            .iter()
            .map(|item| (item.0, item.1))
            .collect::<Vec<_>>();

        let bundle = cx
            .get_fluent_bundle(message.from.as_ref().unwrap().language_code.as_deref())
            .await;

        let (text, used_hashes) = utils::build_alternate_response(&bundle, items);

        drop(action);

        if used_hashes.is_empty() {
            cx.send_generic_reply(message, "reverse-no-results").await?;
            return Ok(());
        }

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: text.clone(),
            disable_web_page_preview: Some(true),
            reply_to_message_id: Some(reply_to_id),
            ..Default::default()
        };

        let sent = cx.bot.make_request(&send_message).await?;

        let matches = FutureRetry::new(
            || cx.fuzzysearch.lookup_hashes(&used_hashes, Some(10)),
            utils::Retry::new(3),
        )
        .await
        .map(|(matches, _attempts)| matches)
        .map_err(|(err, _attempts)| err)?;

        if matches.is_empty() {
            cx.send_generic_reply(message, "reverse-no-results").await?;
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

        let (updated_text, _used_hashes) = utils::build_alternate_response(&bundle, items);

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

        cx.bot
            .make_request(&edit)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    async fn is_valid_admin_group(
        &self,
        cx: &Context,
        message: &tgbotapi::Message,
        bot_needs_admin: bool,
    ) -> Result<bool, Error> {
        use tgbotapi::ChatMemberStatus::*;

        if !message.chat.chat_type.is_group() {
            cx.send_generic_reply(message, "automatic-enable-not-group")
                .await?;
            return Ok(false);
        }

        let user = message.from.as_ref().unwrap();

        // As of the Bot API 5.1, Telegram can now proactively send updates
        // about user information. There is another handler that listens for
        // these changes and saves them to the database. When possible we should
        // use these saved values instead of making more requests.

        let user_is_admin = match models::ChatAdmin::is_admin(&cx.pool, &message.chat, user).await?
        {
            Some(is_admin) => is_admin,
            _ => {
                let get_chat_member = GetChatMember {
                    chat_id: message.chat_id(),
                    user_id: user.id,
                };
                let chat_member = cx.bot.make_request(&get_chat_member).await?;

                matches!(chat_member.status, Administrator | Creator)
            }
        };

        if !user_is_admin {
            cx.send_generic_reply(message, "automatic-enable-not-admin")
                .await?;
            return Ok(false);
        }

        if !bot_needs_admin {
            return Ok(true);
        }

        let bot_is_admin =
            match models::ChatAdmin::is_admin(&cx.pool, &message.chat, &cx.bot_user).await? {
                Some(is_admin) => is_admin,
                _ => {
                    let get_chat_member = GetChatMember {
                        chat_id: message.chat_id(),
                        user_id: cx.bot_user.id,
                    };
                    let bot_member = cx.bot.make_request(&get_chat_member).await?;

                    // Already fetching it, should save it for trying to delete summoning
                    // messages.
                    models::GroupConfig::set(
                        &cx.pool,
                        models::GroupConfigKey::HasDeletePermission,
                        &message.chat,
                        bot_member.can_delete_messages.unwrap_or(false),
                    )
                    .await?;

                    matches!(bot_member.status, Administrator | Creator)
                }
            };

        if !bot_is_admin {
            cx.send_generic_reply(message, "automatic-enable-bot-not-admin")
                .await?;
            return Ok(false);
        }

        Ok(true)
    }

    async fn enable_group_source(
        &self,
        cx: &Context,
        message: &tgbotapi::Message,
    ) -> Result<(), Error> {
        if !self.is_valid_admin_group(cx, message, true).await? {
            return Ok(());
        }

        let result =
            models::GroupConfig::get(&cx.pool, models::GroupConfigKey::GroupAdd, &message.chat)
                .await?
                .unwrap_or(false);

        models::GroupConfig::set(
            &cx.pool,
            models::GroupConfigKey::GroupAdd,
            &message.chat,
            !result,
        )
        .await?;

        let name = if !result {
            "automatic-enable-success"
        } else {
            "automatic-disable"
        };

        cx.send_generic_reply(message, name).await?;

        Ok(())
    }

    async fn group_nopreviews(
        &self,
        cx: &Context,
        message: &tgbotapi::Message,
    ) -> Result<(), Error> {
        if !self.is_valid_admin_group(cx, message, false).await? {
            return Ok(());
        }

        let result = models::GroupConfig::get(
            &cx.pool,
            models::GroupConfigKey::GroupNoPreviews,
            &message.chat,
        )
        .await?
        .unwrap_or(true);

        models::GroupConfig::set(
            &cx.pool,
            models::GroupConfigKey::GroupNoPreviews,
            &message.chat,
            !result,
        )
        .await?;

        let name = if !result {
            "automatic-preview-enable"
        } else {
            "automatic-preview-disable"
        };

        cx.send_generic_reply(message, name).await?;

        Ok(())
    }

    async fn group_noalbums(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        if !self.is_valid_admin_group(cx, message, false).await? {
            return Ok(());
        }

        let result = models::GroupConfig::get(
            &cx.pool,
            models::GroupConfigKey::GroupNoAlbums,
            &message.chat,
        )
        .await?
        .unwrap_or(false);

        models::GroupConfig::set(
            &cx.pool,
            models::GroupConfigKey::GroupNoAlbums,
            &message.chat,
            !result,
        )
        .await?;

        let name = if result {
            "automatic-album-enable"
        } else {
            "automatic-album-disable"
        };

        cx.send_generic_reply(message, name).await?;

        Ok(())
    }

    async fn manage(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let from = match message.from.as_ref() {
            Some(from) => from,
            None => return Ok(()),
        };

        if !cx
            .unleash
            .is_enabled(Features::ManageWeb, Some(&cx.unleash_context(from)), false)
        {
            cx.send_generic_reply(message, "manage-disabled").await?;
            return Ok(());
        }

        let get_chat_member = GetChatMember {
            chat_id: message.chat_id(),
            user_id: from.id,
        };
        let chat_member = cx.bot.make_request(&get_chat_member).await?;

        models::ChatAdmin::update_chat(&cx.pool, &message.chat, from, &chat_member.status, None)
            .await?;

        let bundle = cx
            .get_fluent_bundle(
                message
                    .from
                    .as_ref()
                    .and_then(|user| user.language_code.as_deref()),
            )
            .await;

        let button_text = utils::get_message(&bundle, "manage-button", None);
        let message_text = utils::get_message(&bundle, "manage-message", None);

        let markup =
            tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: vec![vec![tgbotapi::InlineKeyboardButton {
                    text: button_text,
                    login_url: Some(tgbotapi::LoginUrl {
                        url: format!("{}/manage/login", cx.config.public_endpoint),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]],
            });

        let message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            text: message_text,
            reply_to_message_id: Some(message.message_id),
            reply_markup: Some(markup),
            ..Default::default()
        };

        cx.bot.make_request(&message).await?;

        Ok(())
    }

    async fn trace(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let mut resp = String::new();

        let message = if let Some(reply_to_message) = message.reply_to_message.as_ref() {
            writeln!(resp, "Looking at message {}\n", reply_to_message.message_id).unwrap();
            reply_to_message
        } else {
            message
        };

        let links: Vec<_> = cx
            .finder
            .links(message.text.as_deref().unwrap_or_default())
            .chain(
                cx.finder
                    .links(message.caption.as_deref().unwrap_or_default()),
            )
            .collect();
        writeln!(resp, "Found {} links", links.len()).unwrap();

        for (pos, link) in links.into_iter().enumerate() {
            let link = link.as_str();
            writeln!(resp, "Link {pos}: {link}").unwrap();

            let site_id = cx.sites.iter().find_map(|site| site.url_id(link));

            if let Some(site_id) = site_id {
                writeln!(resp, "Image ID: <pre>{site_id}</pre>").unwrap();
            } else {
                writeln!(resp, "No image ID").unwrap();
            }

            for site in cx.sites.iter() {
                if !site.url_supported(link).await {
                    continue;
                }

                for (pos, image) in site
                    .get_images(None, link)
                    .await?
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    writeln!(
                        resp,
                        "Image {} ({}, {}): {}",
                        pos, image.site_name, image.file_type, image.url
                    )
                    .unwrap();
                }

                break;
            }

            writeln!(resp).unwrap();
        }

        if let Some(photo_sizes) = message.photo.as_ref() {
            writeln!(resp, "Found photo with {} sizes", photo_sizes.len()).unwrap();
            let best_size = utils::find_best_photo(photo_sizes).unwrap();
            writeln!(
                resp,
                "Best size was {}x{}",
                best_size.width, best_size.height
            )
            .unwrap();

            let (searched_hash, fuzzysearch_matches) =
                utils::match_image(&cx.bot, &cx.redis, &cx.fuzzysearch, best_size, Some(3)).await?;
            writeln!(resp, "Photo had hash <pre>{searched_hash}</pre>").unwrap();
            writeln!(resp, "Discovered {} results:", fuzzysearch_matches.len(),).unwrap();

            for file in fuzzysearch_matches {
                writeln!(resp, "{} - {}", file.url(), file.distance.unwrap_or(0)).unwrap();
            }
        }

        if message.document.is_some() || message.animation.is_some() {
            writeln!(resp, "Detected document or animation, ignoring.").unwrap();
        }

        let send_message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            text: resp,
            disable_web_page_preview: Some(true),
            reply_to_message_id: Some(message.message_id),
            allow_sending_without_reply: Some(true),
            parse_mode: Some(tgbotapi::requests::ParseMode::Html),
            ..Default::default()
        };
        cx.bot.make_request(&send_message).await?;

        Ok(())
    }
}
