use std::{ops::Add, sync::Arc};

use fluent_bundle::FluentArgs;
use rusoto_s3::S3;
use tracing::Instrument;

use crate::{execute::telegram::Context, models, sites::PostInfo, utils, Error};

#[tracing::instrument(skip(cx, job), fields(job_id = job.id(), chat_id))]
pub async fn process_group_photo(cx: Arc<Context>, job: faktory::Job) -> Result<(), Error> {
    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    let message: tgbotapi::Message = serde_json::value::from_value(data)?;
    tracing::Span::current().record("chat_id", &message.chat.id);
    let photo_sizes = match &message.photo {
        Some(sizes) => sizes,
        _ => return Ok(()),
    };

    tracing::trace!("got enqueued message: {:?}", message);

    if let Err(err) = super::store_linked_chat(&cx, &message).await {
        tracing::error!(
            "could not update bot knowledge of chat linked chat: {:?}",
            err
        );
    }

    match models::GroupConfig::get(&cx.pool, models::GroupConfigKey::GroupAdd, &message.chat)
        .await?
    {
        Some(true) => tracing::debug!("group wants automatic sources"),
        _ => {
            tracing::trace!("group sourcing disabled, skipping message");
            return Ok(());
        }
    }

    if message.media_group_id.is_some() {
        tracing::debug!("message is part of media group, passing message");

        let data = serde_json::to_value(message)?;
        let job = faktory::Job::new("group_mediagroup_message", vec![data])
            .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);

        cx.faktory.enqueue_job(job, None).await?;

        return Ok(());
    }

    match super::is_controlled_channel(&cx, &message).await {
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

    let best_photo = utils::find_best_photo(photo_sizes).unwrap();
    let (searched_hash, mut matches) =
        utils::match_image(&cx.bot, &cx.redis, &cx.fuzzysearch, best_photo, Some(3)).await?;
    utils::sort_results(&cx.pool, message.from.as_ref().unwrap(), &mut matches).await?;

    let wanted_matches = matches
        .iter()
        .filter(|m| m.distance.unwrap() <= super::MAX_SOURCE_DISTANCE)
        .collect::<Vec<_>>();

    if wanted_matches.is_empty() {
        tracing::debug!("found no matches for group image");
        return Ok(());
    }

    let links = utils::extract_links(&message);
    let mut sites = cx.sites.lock().await;

    if wanted_matches
        .iter()
        .any(|m| utils::link_was_seen(&sites, &links, &m.url()))
    {
        tracing::debug!("group message already contained valid links");
        return Ok(());
    }

    if !links.is_empty() {
        let mut results: Vec<PostInfo> = Vec::new();
        let _ = utils::find_images(
            &tgbotapi::User::default(),
            links,
            &mut sites,
            &cx.redis,
            &mut |info| {
                results.extend(info.results);
            },
        )
        .await;

        let urls: Vec<_> = results
            .iter()
            .map::<&str, _>(|result| &result.url)
            .collect();
        if utils::has_similar_hash(searched_hash, &urls).await {
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
    if other_matches <= 1 && twitter_matches >= super::NOISY_SOURCE_COUNT {
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

    let bundle = cx.get_fluent_bundle(lang).await;

    let text = {
        if wanted_matches.len() == 1 {
            let mut args = FluentArgs::new();
            let m = wanted_matches.first().unwrap();
            args.set("link", m.url());

            if let Some(rating) = utils::get_rating_bundle_name(&m.rating) {
                let rating = utils::get_message(&bundle, rating, None);
                args.set("rating", rating);
                utils::get_message(&bundle, "automatic-single", Some(args))
            } else {
                utils::get_message(&bundle, "automatic-single-unknown", Some(args))
            }
        } else {
            let mut buf = String::new();

            buf.push_str(&utils::get_message(&bundle, "automatic-multiple", None));
            buf.push('\n');

            for result in wanted_matches {
                let mut args = FluentArgs::new();
                args.set("link", result.url());

                let message = if let Some(rating) = utils::get_rating_bundle_name(&result.rating) {
                    let rating = utils::get_message(&bundle, rating, None);
                    args.set("rating", rating);
                    utils::get_message(&bundle, "automatic-multiple-result", Some(args))
                } else {
                    utils::get_message(&bundle, "automatic-multiple-result-unknown", Some(args))
                };

                buf.push_str(&message);
                buf.push('\n');
            }

            buf
        }
    };

    let data = serde_json::to_value(&super::GroupSource {
        chat_id: message.chat.id.to_string(),
        reply_to_message_id: message.message_id,
        text,
    })?;

    let job =
        faktory::Job::new("group_source", vec![data]).on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);

    cx.faktory.enqueue_job(job, None).await?;

    Ok(())
}

#[tracing::instrument(skip(cx, job), fields(job_id = job.id(), chat_id))]
pub async fn process_group_source(cx: Arc<Context>, job: faktory::Job) -> Result<(), Error> {
    use tgbotapi::requests::SendMessage;

    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    tracing::trace!("got enqueued group source: {:?}", data);

    let super::GroupSource {
        chat_id,
        reply_to_message_id,
        text,
    } = serde_json::value::from_value(data.clone())?;
    let chat_id: &str = &chat_id;
    tracing::Span::current().record("chat_id", &chat_id);

    if let Some(at) = super::check_more_time(&cx.redis, chat_id).await {
        tracing::trace!("need to wait more time for this chat: {}", at);

        let mut job = faktory::Job::new("group_source", vec![data])
            .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);
        job.at = Some(at);

        cx.faktory.enqueue_job(job, None).await?;

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

    match cx.bot.make_request(&message).await {
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

            super::needs_more_time(&cx.redis, chat_id, retry_at).await;

            let mut job = faktory::Job::new("group_source", vec![data])
                .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);
            job.at = Some(retry_at);

            cx.faktory.enqueue_job(job, None).await?;

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

#[tracing::instrument(skip(cx, job), fields(job_id = job.id(), chat_id, media_group_id))]
pub async fn process_group_mediagroup_message(
    cx: Arc<Context>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    let message: tgbotapi::Message = serde_json::value::from_value(data)?;
    tracing::Span::current().record("chat_id", &message.chat.id);

    let media_group_id = message.media_group_id.as_deref().unwrap();
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("got media group message");

    let stored_id = models::MediaGroup::add_message(&cx.pool, &message).await?;

    tracing::debug!("queueing group check");

    let data = serde_json::to_value(media_group_id)?;
    let mut job = faktory::Job::new("group_mediagroup_check", vec![data])
        .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);
    job.at = Some(chrono::Utc::now() + chrono::Duration::seconds(10));

    cx.faktory.enqueue_job(job, None).await?;

    let data = serde_json::to_value(stored_id)?;
    let job = faktory::Job::new("group_mediagroup_hash", vec![data])
        .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);

    cx.faktory.enqueue_job(job, None).await?;

    Ok(())
}

#[tracing::instrument(skip(cx, job), fields(job_id = job.id()))]
pub async fn process_group_mediagroup_hash(
    cx: Arc<Context>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    let stored_id: i32 = serde_json::value::from_value(data)?;

    let message = match models::MediaGroup::get_message(&cx.pool, stored_id).await? {
        Some(message) => message,
        None => {
            tracing::debug!("message was removed before hash could be calculated");
            return Ok(());
        }
    };

    tracing::debug!("finding sources for pending media group item");

    let sizes = message.message.photo.as_ref().unwrap();
    let best_photo = utils::find_best_photo(sizes).unwrap();

    let get_file = tgbotapi::requests::GetFile {
        file_id: best_photo.file_id.clone(),
    };

    let file_info = cx.bot.make_request(&get_file).await?;
    let data = cx.bot.download_file(&file_info.file_path.unwrap()).await?;

    if models::GroupConfig::get(
        &cx.pool,
        models::GroupConfigKey::GroupNoAlbums,
        &message.message.chat,
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
            bucket: cx.config.s3_bucket.to_string(),
            content_type: Some(kind.mime_type().into()),
            key: path,
            content_length: Some(data.len() as i64),
            body: Some(data.clone().into()),
            ..Default::default()
        };
        cx.s3.put_object(put).await.unwrap();
    }

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&data))
        .instrument(tracing::debug_span!("hash_bytes"))
        .await??;

    models::FileCache::set(&cx.redis, &best_photo.file_unique_id, hash).await?;

    let mut sources = utils::lookup_single_hash(&cx.fuzzysearch, hash, Some(3)).await?;

    utils::sort_results(
        &cx.pool,
        message.message.from.as_ref().unwrap(),
        &mut sources,
    )
    .await?;

    tracing::debug!("found sources, saving for media group item");

    models::MediaGroup::set_message_sources(&cx.pool, stored_id, sources).await?;

    Ok(())
}

#[tracing::instrument(skip(cx, job), fields(job_id = job.id(), media_group_id))]
pub async fn process_group_mediagroup_check(
    cx: Arc<Context>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    let media_group_id: String = serde_json::from_value(data)?;
    let media_group_id: &str = &media_group_id;
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("checking media group age");

    let last_updated_at = match models::MediaGroup::last_message(&cx.pool, media_group_id).await? {
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
        let mut job = faktory::Job::new("group_mediagroup_check", vec![data])
            .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);
        job.at = Some(chrono::Utc::now() + chrono::Duration::seconds(10));

        cx.faktory.enqueue_job(job, None).await?;

        return Ok(());
    }

    if !models::MediaGroup::sending_message(&cx.pool, media_group_id).await? {
        tracing::info!("media group was already sent");
        return Ok(());
    }

    let messages = models::MediaGroup::get_messages(&cx.pool, media_group_id).await?;
    let first_message = &messages.first().as_ref().unwrap().message;

    let lang_code = first_message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    let bundle = cx.get_fluent_bundle(lang_code).await;

    if models::GroupConfig::get(
        &cx.pool,
        models::GroupConfigKey::GroupNoAlbums,
        &first_message.chat,
    )
    .await?
    .unwrap_or(false)
    {
        tracing::trace!("group doesn't want inline album sources, generating link");

        let data = serde_json::to_value(media_group_id)?;
        let mut job = faktory::Job::new("group_mediagroup_prune", vec![data])
            .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);

        let has_sources = messages.iter().any(|message| {
            !message
                .sources
                .as_ref()
                .map(|sources| sources.is_empty())
                .unwrap_or(true)
        });

        if has_sources {
            tracing::debug!("media group had sources, sending message");

            let link = format!("{}/mg/{}", cx.config.public_endpoint, media_group_id);
            let mut args = FluentArgs::new();
            args.set("link", link);
            let message = utils::get_message(&bundle, "automatic-sources-link", Some(args));

            let send_message = tgbotapi::requests::SendMessage {
                chat_id: first_message.chat_id(),
                reply_to_message_id: Some(first_message.message_id),
                text: message,
                disable_web_page_preview: Some(true),
                disable_notification: Some(true),
                ..Default::default()
            };

            cx.bot.make_request(&send_message).await?;

            job.at = Some(chrono::Utc::now() + chrono::Duration::hours(24));
        } else {
            tracing::debug!("media group had no sources, skipping message and pruning now");

            job.at = None;
        }

        cx.faktory.enqueue_job(job, None).await?;

        return Ok(());
    }

    let mut messages = models::MediaGroup::consume_messages(&cx.pool, media_group_id).await?;
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
        let best_photo = utils::find_best_photo(sizes).unwrap();

        let mut sources =
            utils::match_image(&cx.bot, &cx.redis, &cx.fuzzysearch, best_photo, Some(3))
                .await?
                .1;
        utils::sort_results(
            &cx.pool,
            message.message.from.as_ref().unwrap(),
            &mut sources,
        )
        .await?;
        message.sources = Some(sqlx::types::Json(sources));
    }

    let has_sources = messages.iter().any(|message| {
        !message
            .sources
            .as_ref()
            .map(|sources| sources.is_empty())
            .unwrap_or(true)
    });

    if !has_sources {
        tracing::debug!("media group had no sources, skipping message");
        return Ok(());
    }

    let mut buf = String::new();

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

        let mut args = FluentArgs::new();
        args.set("number", index + 1);

        let image = utils::get_message(&bundle, "automatic-image-number", Some(args));

        buf.push_str(&image);
        buf.push('\n');
        buf.push_str(&urls.join("\n"));
        buf.push_str("\n\n");
    }

    let first_message = &messages.first().as_ref().unwrap().message;

    let send_message = tgbotapi::requests::SendMessage {
        chat_id: first_message.chat_id(),
        reply_to_message_id: Some(first_message.message_id),
        text: buf,
        disable_web_page_preview: Some(true),
        disable_notification: Some(true),
        ..Default::default()
    };

    cx.bot.make_request(&send_message).await?;

    Ok(())
}

#[tracing::instrument(skip(cx, job), fields(job_id = job.id(), media_group_id))]
pub async fn process_group_mediagroup_prune(
    cx: Arc<Context>,
    job: faktory::Job,
) -> Result<(), Error> {
    let data: serde_json::Value = job.args().iter().next().ok_or(Error::Missing)?.to_owned();

    let media_group_id: String = serde_json::from_value(data)?;
    let media_group_id: &str = &media_group_id;
    tracing::Span::current().record("media_group_id", &media_group_id);

    tracing::debug!("pruning media group");

    let messages = models::MediaGroup::get_messages(&cx.pool, media_group_id).await?;

    for message in messages {
        tracing::trace!(
            message_id = message.message.message_id,
            "deleting photo from message"
        );
        let best_photo = utils::find_best_photo(message.message.photo.as_deref().unwrap()).unwrap();

        let path = format!("mg/{}/{}", media_group_id, best_photo.file_id);
        let delete = rusoto_s3::DeleteObjectRequest {
            bucket: cx.config.s3_bucket.to_string(),
            key: path,
            ..Default::default()
        };
        cx.s3.delete_object(delete).await.unwrap();
    }

    models::MediaGroup::purge(&cx.pool, media_group_id).await?;

    tracing::info!("pruned media group");

    Ok(())
}
