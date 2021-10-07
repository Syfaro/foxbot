use anyhow::Context;
use fluent::fluent_args;
use rusoto_s3::S3;
use tgbotapi::requests::GetChat;

use crate::*;
use foxbot_models::{FileCache, GroupConfig, GroupConfigKey, MediaGroup};
use foxbot_utils::has_similar_hash;

#[tracing::instrument(skip(handler, job), fields(job_id = job.id(), chat_id))]
pub async fn process_group_photo(handler: Arc<Handler>, job: faktory::Job) -> Result<(), Error> {
    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let message: tgbotapi::Message = serde_json::value::from_value(data)?;
    tracing::Span::current().record("chat_id", &message.chat.id);
    let photo_sizes = match &message.photo {
        Some(sizes) => sizes,
        _ => return Ok(()),
    };

    tracing::trace!("got enqueued message: {:?}", message);

    if let Err(err) = store_linked_chat(&handler, &message).await {
        tracing::error!(
            "could not update bot knowledge of chat linked chat: {:?}",
            err
        );
    }

    match GroupConfig::get(&handler.conn, message.chat.id, GroupConfigKey::GroupAdd).await? {
        Some(true) => tracing::debug!("group wants automatic sources"),
        _ => {
            tracing::trace!("group sourcing disabled, skipping message");
            return Ok(());
        }
    }

    if message.media_group_id.is_some() {
        tracing::debug!("message is part of media group, passing message");

        let data = serde_json::to_value(message)?;
        let mut job =
            faktory::Job::new("group_mediagroup_message", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();

        handler.enqueue(job).await;

        return Ok(());
    }

    match is_controlled_channel(&handler, &message).await {
        Ok(true) => {
            tracing::debug!("message was forwarded from controlled channel");

            let date = message.forward_date.unwrap_or(message.date);
            let now = chrono::Utc::now();
            let message_date = chrono::DateTime::from_utc(
                chrono::NaiveDateTime::from_timestamp(date, 0),
                chrono::Utc,
            );
            let hours_ago = (now - message_date).num_hours();

            tracing::trace!(hours_ago, "calculated message age");

            // After 6 hours, it's fair game to try and source the post. This is
            // important if a channel has posts that weren't sourced when they
            // were posted, either because the bot wasn't enabled then or the
            // source wasn't discovered yet.
            if hours_ago < 6 {
                tracing::debug!("message was too new to ensure source existence, skipping");
                return Ok(());
            }
        }
        Ok(false) => tracing::trace!("message was from uncontrolled channel, adding source"),
        Err(err) => tracing::error!(
            "could not check if message was forwarded from controlled channel: {:?}",
            err
        ),
    }

    let best_photo = find_best_photo(photo_sizes).unwrap();
    let (searched_hash, mut matches) = match_image(
        &handler.telegram,
        &handler.redis,
        &handler.fuzzysearch,
        best_photo,
        Some(3),
    )
    .await?;
    sort_results(
        &handler.conn,
        message.from.as_ref().unwrap().id,
        &mut matches,
    )
    .await?;

    let wanted_matches = matches
        .iter()
        .filter(|m| m.distance.unwrap() <= MAX_SOURCE_DISTANCE)
        .collect::<Vec<_>>();

    if wanted_matches.is_empty() {
        tracing::debug!("found no matches for group image");
        return Ok(());
    }

    let links = extract_links(&message);
    let mut sites = handler.sites.lock().await;

    if wanted_matches
        .iter()
        .any(|m| link_was_seen(&sites, &links, &m.url()))
    {
        tracing::debug!("group message already contained valid links");
        return Ok(());
    }

    if !links.is_empty() {
        let mut results: Vec<foxbot_sites::PostInfo> = Vec::new();
        let _ = find_images(
            &tgbotapi::User::default(),
            links,
            &mut sites,
            &handler.redis,
            &mut |info| {
                results.extend(info.results);
            },
        )
        .await;

        let urls: Vec<_> = results
            .iter()
            .map::<&str, _>(|result| &result.url)
            .collect();
        if has_similar_hash(searched_hash, &urls).await {
            tracing::debug!("url in post contained similar hash");
            return Ok(());
        }
    }

    drop(sites);

    let twitter_matches = wanted_matches
        .iter()
        .filter(|m| matches!(m.site_info, Some(fuzzysearch::SiteInfo::Twitter)))
        .count();
    let other_matches = wanted_matches.len() - twitter_matches;

    // Prevents memes from getting a million links in chat
    if other_matches <= 1 && twitter_matches >= NOISY_SOURCE_COUNT {
        tracing::trace!(
            twitter_matches,
            other_matches,
            "had too many matches, ignoring"
        );
        return Ok(());
    }

    let lang = message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    let text = handler
        .get_fluent_bundle(lang, |bundle| {
            if wanted_matches.len() == 1 {
                let mut args = fluent::FluentArgs::new();
                let m = wanted_matches.first().unwrap();
                args.insert("link", m.url().into());

                if let Some(rating) = get_rating_bundle_name(&m.rating) {
                    let rating = get_message(bundle, rating, None).unwrap();
                    args.insert("rating", rating.into());
                    get_message(bundle, "automatic-single", Some(args)).unwrap()
                } else {
                    get_message(bundle, "automatic-single-unknown", Some(args)).unwrap()
                }
            } else {
                let mut buf = String::new();

                buf.push_str(&get_message(bundle, "automatic-multiple", None).unwrap());
                buf.push('\n');

                for result in wanted_matches {
                    let mut args = fluent::FluentArgs::new();
                    args.insert("link", result.url().into());

                    let message = if let Some(rating) = get_rating_bundle_name(&result.rating) {
                        let rating = get_message(bundle, rating, None).unwrap();
                        args.insert("rating", rating.into());
                        get_message(bundle, "automatic-multiple-result", Some(args)).unwrap()
                    } else {
                        get_message(bundle, "automatic-multiple-result-unknown", Some(args))
                            .unwrap()
                    };

                    buf.push_str(&message);
                    buf.push('\n');
                }

                buf
            }
        })
        .await;

    let data = serde_json::to_value(&GroupSource {
        chat_id: message.chat.id.to_string(),
        reply_to_message_id: message.message_id,
        text,
    })?;

    let mut job = faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
    job.custom = get_faktory_custom();

    handler.enqueue(job).await;

    Ok(())
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id(), chat_id))]
pub async fn process_group_source(handler: Arc<Handler>, job: faktory::Job) -> Result<(), Error> {
    use tgbotapi::requests::SendMessage;

    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    tracing::trace!("got enqueued group source: {:?}", data);

    let GroupSource {
        chat_id,
        reply_to_message_id,
        text,
    } = serde_json::value::from_value(data.clone())?;
    let chat_id: &str = &chat_id;
    tracing::Span::current().record("chat_id", &chat_id);

    if let Some(at) = check_more_time(&handler.redis, chat_id).await {
        tracing::trace!("need to wait more time for this chat: {}", at);

        let mut job = faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
        job.at = Some(at);
        job.custom = get_faktory_custom();

        handler.enqueue(job).await;

        return Ok(());
    }

    let message = SendMessage {
        chat_id: chat_id.into(),
        reply_to_message_id: Some(reply_to_message_id),
        disable_web_page_preview: Some(true),
        disable_notification: Some(true),
        text,
        ..Default::default()
    };

    match handler.telegram.make_request(&message).await {
        Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            parameters:
                Some(tgbotapi::ResponseParameters {
                    retry_after: Some(retry_after),
                    ..
                }),
            ..
        })) => {
            tracing::warn!(retry_after, "rate limiting, re-enqueuing");

            let now = chrono::offset::Utc::now();
            let retry_at = now.add(chrono::Duration::seconds(retry_after as i64));

            needs_more_time(&handler.redis, chat_id, retry_at).await;

            let mut job =
                faktory::Job::new("group_source", vec![data]).on_queue("foxbot_background");
            job.at = Some(retry_at);
            job.custom = get_faktory_custom();

            handler.enqueue(job).await;

            Ok(())
        }
        Ok(_)
        | Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            error_code: Some(400),
            ..
        })) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Check if one chat is linked to another chat.
///
/// Returns if the message was sent to a chat linked to a different chat.
#[tracing::instrument(skip(handler, message))]
pub async fn store_linked_chat(
    handler: &Handler,
    message: &tgbotapi::Message,
) -> anyhow::Result<bool> {
    if let Some(linked_chat) = GroupConfig::get::<Option<i64>>(
        &handler.conn,
        message.chat.id,
        GroupConfigKey::HasLinkedChat,
    )
    .await
    .context("could not check if had known linked chat")?
    {
        tracing::trace!("already knew if had linked chat");
        return Ok(linked_chat.is_some());
    }

    let chat = handler
        .telegram
        .make_request(&GetChat {
            chat_id: message.chat_id(),
        })
        .await
        .context("could not get chat information")?;

    if let Some(linked_chat_id) = chat.linked_chat_id {
        tracing::debug!(linked_chat_id, "discovered linked chat");
    } else {
        tracing::debug!("no linked chat found");
    }

    GroupConfig::set(
        &handler.conn,
        GroupConfigKey::HasLinkedChat,
        message.chat.id,
        chat.linked_chat_id,
    )
    .await
    .context("could not save linked chat information")?;

    Ok(chat.linked_chat_id.is_some())
}

/// Check if the message was a forward from a chat, and if it was, check if we
/// have edit permissions in that channel. If we do, we can skip this message as
/// it will get a source applied automatically.
#[tracing::instrument(skip(handler, message), fields(forward_from_chat_id))]
async fn is_controlled_channel(
    handler: &Handler,
    message: &tgbotapi::Message,
) -> anyhow::Result<bool> {
    // If it wasn't forwarded from a channel, there's no way it's from a
    // controlled channel.
    let forward_from_chat = match &message.forward_from_chat {
        Some(chat) => chat,
        None => return Ok(false),
    };

    tracing::Span::current().record("forward_from_chat_id", &forward_from_chat.id);

    let can_edit = match GroupConfig::get::<bool>(
        &handler.conn,
        forward_from_chat.id,
        GroupConfigKey::CanEditChannel,
    )
    .await?
    {
        Some(true) => {
            tracing::trace!("message was forwarded from chat with edit permissions");
            true
        }
        Some(false) => {
            tracing::trace!("message was forwarded from chat without edit permissions");
            false
        }
        None => {
            tracing::trace!("message was forwarded from unknown chat");
            false
        }
    };

    Ok(can_edit)
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id(), chat_id, media_group_id))]
pub async fn process_group_mediagroup_message(
    handler: Arc<Handler>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let message: tgbotapi::Message = serde_json::value::from_value(data)?;
    tracing::Span::current().record("chat_id", &message.chat.id);

    let media_group_id = message.media_group_id.as_deref().unwrap();
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("got media group message");

    let stored_id = MediaGroup::add_message(&handler.conn, &message).await?;

    tracing::debug!("queueing group check");

    let data = serde_json::to_value(media_group_id)?;
    let mut job =
        faktory::Job::new("group_mediagroup_check", vec![data]).on_queue("foxbot_background");
    job.custom = get_faktory_custom();
    job.at = Some(chrono::Utc::now() + chrono::Duration::seconds(10));

    handler.enqueue(job).await;

    let data = serde_json::to_value(stored_id)?;
    let mut job =
        faktory::Job::new("group_mediagroup_hash", vec![data]).on_queue("foxbot_background");
    job.custom = get_faktory_custom();

    handler.enqueue(job).await;

    Ok(())
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id()))]
pub async fn process_group_mediagroup_hash(
    handler: Arc<Handler>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let stored_id: i32 = serde_json::value::from_value(data)?;

    let message = match MediaGroup::get_message(&handler.conn, stored_id).await? {
        Some(message) => message,
        None => {
            tracing::debug!("message was removed before hash could be calculated");
            return Ok(());
        }
    };

    tracing::debug!("finding sources for pending media group item");

    let sizes = message.message.photo.unwrap();
    let best_photo = find_best_photo(&sizes).unwrap();

    let get_file = tgbotapi::requests::GetFile {
        file_id: best_photo.file_id.clone(),
    };

    let file_info = handler
        .telegram
        .make_request(&get_file)
        .await
        .context("unable to request file info from telegram")?;
    let data = handler
        .telegram
        .download_file(&file_info.file_path.unwrap())
        .await
        .context("unable to download file from telegram")?;

    if GroupConfig::get(
        &handler.conn,
        message.message.chat.id,
        GroupConfigKey::GroupNoAlbums,
    )
    .await?
    .unwrap_or(false)
    {
        tracing::debug!("group doesn't want inline album sources, uploading image to cdn bucket");

        let kind = infer::get(&data).unwrap();

        let path = format!(
            "mg/{}/{}",
            message.message.media_group_id.as_ref().unwrap(),
            best_photo.file_id,
        );
        let put = rusoto_s3::PutObjectRequest {
            acl: Some("download".into()),
            bucket: handler.config.s3_bucket.to_string(),
            content_type: Some(kind.mime_type().into()),
            key: path,
            content_length: Some(data.len() as i64),
            body: Some(data.clone().into()),
            ..Default::default()
        };
        handler.s3.put_object(put).await.unwrap();
    }

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&data))
        .instrument(tracing::debug_span!("hash_bytes"))
        .await
        .context("unable to spawn blocking")?
        .context("unable to hash bytes")?;

    FileCache::set(&handler.redis, &best_photo.file_unique_id, hash)
        .await
        .context("unable to set file cache")?;

    let mut sources = lookup_single_hash(&handler.fuzzysearch, hash, Some(3)).await?;

    sort_results(
        &handler.conn,
        message.message.from.as_ref().unwrap().id,
        &mut sources,
    )
    .await?;

    tracing::debug!("found sources, saving for media group item");

    MediaGroup::set_message_sources(&handler.conn, stored_id, sources).await;

    Ok(())
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id(), media_group_id))]
pub async fn process_group_mediagroup_check(
    handler: Arc<Handler>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let media_group_id: String = serde_json::from_value(data)?;
    let media_group_id: &str = &media_group_id;
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("checking media group age");

    let last_updated_at = match MediaGroup::last_message(&handler.conn, media_group_id).await? {
        Some(last_updated_at) => last_updated_at,
        None => {
            tracing::debug!("media group had already been processed");
            return Ok(());
        }
    };

    tracing::debug!("media group was last updated at {}", last_updated_at);

    if chrono::Utc::now() - last_updated_at < chrono::Duration::seconds(10) {
        tracing::debug!("group was updated more recently than 10 seconds, requeueing check");

        let data = serde_json::to_value(media_group_id)?;
        let mut job =
            faktory::Job::new("group_mediagroup_check", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();
        job.at = Some(chrono::Utc::now() + chrono::Duration::seconds(10));

        handler.enqueue(job).await;

        return Ok(());
    }

    if !MediaGroup::sending_message(&handler.conn, media_group_id).await? {
        tracing::info!("media group was already sent");
        return Ok(());
    }

    let messages = MediaGroup::get_messages(&handler.conn, media_group_id).await?;
    let first_message = &messages.first().as_ref().unwrap().message;

    let lang_code = first_message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    if GroupConfig::get(
        &handler.conn,
        first_message.chat.id,
        GroupConfigKey::GroupNoAlbums,
    )
    .await?
    .unwrap_or(false)
    {
        tracing::trace!("group doesn't want inline album sources, generating link");

        let data = serde_json::to_value(media_group_id)?;
        let mut job =
            faktory::Job::new("group_mediagroup_prune", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();

        let has_sources = messages
            .iter()
            .any(|message| !message.sources.as_deref().unwrap_or_default().is_empty());

        if has_sources {
            tracing::debug!("media group had sources, sending message");

            let link = format!("{}/mg/{}", handler.config.internet_url, media_group_id);
            let message = handler
                .get_fluent_bundle(lang_code, |bundle| {
                    get_message(
                        bundle,
                        "automatic-sources-link",
                        Some(fluent_args!["link" => link]),
                    )
                    .unwrap()
                })
                .await;

            let send_message = tgbotapi::requests::SendMessage {
                chat_id: first_message.chat_id(),
                reply_to_message_id: Some(first_message.message_id),
                text: message,
                disable_web_page_preview: Some(true),
                disable_notification: Some(true),
                ..Default::default()
            };

            handler.telegram.make_request(&send_message).await?;

            job.at = Some(chrono::Utc::now() + chrono::Duration::hours(24));
        } else {
            tracing::debug!("media group had no sources, skipping message and pruning now");

            job.at = None;
        }

        handler.enqueue(job).await;

        return Ok(());
    }

    let mut messages = MediaGroup::consume_messages(&handler.conn, media_group_id).await?;
    if messages.is_empty() {
        tracing::info!("messages was empty, must have already processed");
        return Ok(());
    }

    messages.sort_by(|a, b| a.message.message_id.cmp(&b.message.message_id));

    tracing::debug!("found messages");

    for message in &mut messages {
        if message.sources.is_some() {
            continue;
        }

        tracing::debug!(
            "looking up sources for message {}",
            message.message.message_id
        );

        let sizes = message.message.photo.as_ref().unwrap();
        let best_photo = find_best_photo(sizes).unwrap();

        let mut sources = match_image(
            &handler.telegram,
            &handler.redis,
            &handler.fuzzysearch,
            best_photo,
            Some(3),
        )
        .await?
        .1;
        sort_results(
            &handler.conn,
            message.message.from.as_ref().unwrap().id,
            &mut sources,
        )
        .await?;
        message.sources = Some(sources);
    }

    let has_sources = messages
        .iter()
        .any(|message| !message.sources.as_deref().unwrap_or_default().is_empty());

    if !has_sources {
        tracing::debug!("media group had no sources, skipping message");
        return Ok(());
    }

    let mut buf = String::new();

    handler
        .get_fluent_bundle(lang_code, |bundle| {
            for (index, message) in messages.iter().enumerate() {
                let urls = message
                    .sources
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|file| file.url())
                    .take(2)
                    .collect::<Vec<_>>();
                if urls.is_empty() {
                    continue;
                }

                let image = get_message(
                    bundle,
                    "automatic-image-number",
                    Some(fluent_args!["number" => index + 1]),
                )
                .unwrap();

                buf.push_str(&image);
                buf.push('\n');
                buf.push_str(&urls.join("\n"));
                buf.push_str("\n\n");
            }
        })
        .await;

    let first_message = &messages.first().as_ref().unwrap().message;

    let send_message = tgbotapi::requests::SendMessage {
        chat_id: first_message.chat_id(),
        reply_to_message_id: Some(first_message.message_id),
        text: buf,
        disable_web_page_preview: Some(true),
        disable_notification: Some(true),
        ..Default::default()
    };

    handler.telegram.make_request(&send_message).await?;

    Ok(())
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id(), media_group_id))]
pub async fn process_group_mediagroup_prune(
    handler: Arc<Handler>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let media_group_id: String = serde_json::from_value(data)?;
    let media_group_id: &str = &media_group_id;
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("pruning media group");

    let messages = MediaGroup::get_messages(&handler.conn, media_group_id).await?;

    for message in messages {
        tracing::trace!(
            message_id = message.message.message_id,
            "deleting photo from message"
        );
        let best_photo = find_best_photo(message.message.photo.as_deref().unwrap()).unwrap();

        let path = format!("mg/{}/{}", media_group_id, best_photo.file_id);
        let delete = rusoto_s3::DeleteObjectRequest {
            bucket: handler.config.s3_bucket.to_string(),
            key: path,
            ..Default::default()
        };
        handler.s3.delete_object(delete).await.unwrap();
    }

    MediaGroup::purge_media_group(&handler.conn, media_group_id).await?;

    tracing::info!("pruned media group");

    Ok(())
}
