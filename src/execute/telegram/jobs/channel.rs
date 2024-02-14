use std::{ops::Add, sync::Arc};

use redis::AsyncCommands;
use tgbotapi::requests::GetChatMember;

use crate::{
    execute::{
        telegram::Context,
        telegram_jobs::{ChannelEditJob, MessageEdit},
    },
    models,
    sites::PostInfo,
    utils, Error,
};

#[tracing::instrument(skip(cx, job, message), fields(job_id = job.id()))]
pub async fn process_channel_update(
    cx: Arc<Context>,
    job: faktory::Job,
    message: tgbotapi::Message,
) -> Result<(), Error> {
    tracing::trace!("got enqueued message: {:?}", message);

    let has_linked_chat = super::store_linked_chat(&cx, &message).await?;

    // Photos should exist for job to be enqueued.
    let sizes = match &message.photo {
        Some(photo) => photo,
        _ => return Ok(()),
    };

    if let Err(err) = store_channel_edit(&cx, &message).await {
        tracing::error!(
            "could not update bot knowledge of channel edit permissions: {:?}",
            err
        );
    }

    let allow_nsfw = models::GroupConfig::get(
        &cx.pool,
        models::GroupConfigKey::Nsfw,
        models::Chat::Telegram(message.chat.id),
    )
    .await?
    .unwrap_or(true);

    let file =
        utils::find_best_photo(sizes).ok_or_else(|| Error::missing("channel update photo"))?;
    let (searched_hash, mut matches) = utils::match_image(
        &cx.bot,
        &cx.redis,
        &cx.fuzzysearch,
        file,
        Some(3),
        allow_nsfw,
    )
    .await?;

    // Only keep matches with a distance of 3 or less
    matches.retain(|m| m.distance.unwrap_or(10) <= 3);

    if matches.is_empty() {
        tracing::debug!("unable to find sources for image");
        return Ok(());
    }

    let links = utils::extract_links(&message);

    // If any matches contained a link we found in the message, skip adding
    // a source.
    if matches
        .iter()
        .any(|file| utils::link_was_seen(&cx.sites, &links, &file.url()))
    {
        tracing::trace!("post already contained valid source url");
        return Ok(());
    }

    if !links.is_empty() {
        let mut results: Vec<PostInfo> = Vec::new();
        let _ = utils::find_images(
            &tgbotapi::User::default(),
            links,
            &cx.sites,
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

    if already_had_source(&cx.redis, &message, &matches).await? {
        tracing::trace!("post group already contained source url");
        return Ok(());
    }

    // Keep order of sites consistent.
    utils::sort_results_by(&models::Sites::default_order(), &mut matches, true);

    let firsts = utils::first_of_each_site(&matches)
        .into_iter()
        .map(|(site, file)| (site, file.url()))
        .collect();

    let job = ChannelEditJob {
        message_edit: MessageEdit {
            chat_id: message.chat.id.to_string(),
            message_id: message.message_id,
            media_group_id: message.media_group_id,
            firsts,
            previous_text: message.caption,
            entities: message.caption_entities,
        },
        has_linked_chat,
    };

    cx.faktory.enqueue_job(job, None).await?;

    Ok(())
}

#[tracing::instrument(skip(cx, job, message_edit), fields(job_id = job.id()))]
pub async fn process_channel_edit(
    cx: Arc<Context>,
    job: faktory::Job,
    (message_edit, has_linked_chat): (MessageEdit, bool),
) -> Result<(), Error> {
    tracing::trace!("got enqueued edit: {:?}", message_edit);

    let chat_id: &str = &message_edit.chat_id;

    if let Some(at) = super::check_more_time(&cx.redis, chat_id).await {
        tracing::trace!("need to wait more time for this chat: {}", at);

        let job = ChannelEditJob {
            message_edit,
            has_linked_chat,
        };

        cx.faktory.enqueue_job_at(job, None, Some(at)).await?;

        return Ok(());
    }

    tracing::trace!(has_linked_chat, "evaluating if chat is linked");

    let always_use_captions = if let Ok(chat_id) = chat_id.parse::<i64>() {
        models::GroupConfig::get(
            &cx.pool,
            models::GroupConfigKey::ChannelCaption,
            models::Chat::Telegram(chat_id),
        )
        .await?
    } else {
        tracing::error!("chat_id was not i64: {chat_id}");
        None
    };

    tracing::trace!(
        always_use_captions,
        "determined if channel always wants captions"
    );

    // If this channel has a linked chat or the photo was part of a media group
    // and the user has not explicitly changed the always use captions setting,
    // set a caption with the sources.
    let resp = if (has_linked_chat || message_edit.media_group_id.is_some())
        && always_use_captions != Some(false)
    {
        let sources = message_edit
            .firsts
            .iter()
            .map(|(_site, url)| url.to_owned())
            .collect::<Vec<_>>()
            .join("\n");

        let caption = if let Some(original_caption) = &message_edit.previous_text {
            format!("{original_caption}\n\n{sources}")
        } else {
            sources
        };

        let edit_caption_markup = tgbotapi::requests::EditMessageCaption {
            chat_id: chat_id.into(),
            message_id: Some(message_edit.message_id),
            caption: Some(caption),
            caption_entities: message_edit.entities.clone(),
            ..Default::default()
        };

        cx.bot.make_request(&edit_caption_markup).await
    // Not a media group, we should create an inline keyboard.
    } else {
        let buttons: Vec<_> = message_edit
            .firsts
            .iter()
            .map(|(site, url)| tgbotapi::InlineKeyboardButton {
                text: site.as_str().to_string(),
                url: Some(url.to_owned()),
                ..Default::default()
            })
            .collect();

        let buttons = if buttons.len() % 2 == 0 {
            buttons.chunks(2).map(|chunk| chunk.to_vec()).collect()
        } else {
            buttons.chunks(1).map(|chunk| chunk.to_vec()).collect()
        };

        let markup = tgbotapi::InlineKeyboardMarkup {
            inline_keyboard: buttons,
        };

        let edit_reply_markup = tgbotapi::requests::EditMessageReplyMarkup {
            chat_id: chat_id.into(),
            message_id: Some(message_edit.message_id),
            reply_markup: Some(tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(
                markup,
            )),
            ..Default::default()
        };

        cx.bot.make_request(&edit_reply_markup).await
    };

    match resp {
        // When we get rate limited, mark the job as successful and enqueue
        // it again after the retry after period.
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

            let job = ChannelEditJob {
                message_edit,
                has_linked_chat,
            };

            cx.faktory.enqueue_job_at(job, None, Some(retry_at)).await?;

            Ok(())
        }
        // It seems like the bot gets updates from channels, but is unable
        // to update them. Telegram often gives us a
        // 'Bad Request: MESSAGE_ID_INVALID' response here.
        //
        // The corresponding updates have inline keyboard markup, suggesting
        // that they were generated by a bot.
        //
        // I'm not sure if there's any way to detect this before processing
        // an update, so ignore these errors.
        Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            error_code: Some(400),
            description,
            ..
        })) => {
            tracing::warn!("got 400 error, ignoring: {:?}", description);

            Ok(())
        }
        // If permissions have changed (bot was removed from channel, etc.)
        // we may no longer be allowed to process this update. There's
        // nothing else we can do so mark it as successful.
        Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
            error_code: Some(403),
            description,
            ..
        })) => {
            tracing::warn!("got 403 error, ignoring: {:?}", description);

            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Telegram only shows a caption on a media group if there is a single caption
/// anywhere in the group. When users upload a group, we need to check if we can
/// only set a single source to make the link more visible. This can be done by
/// ensuring our source has been previouly used in the media group.
///
/// For our implementation, this is done by maintaining a Redis set of every
/// source previously displayed. If adding our source links returns fewer
/// inserted than we had, it means a link was previously used and therefore we
/// do not have to set a source.
///
/// Because Telegram doesn't send media groups at once, we have to store these
/// values until we're sure the group is over. In this case, we will store
/// values for 300 seconds.
///
/// No link normalization is required here because all links are already
/// normalized when coming from FuzzySearch.
async fn already_had_source(
    redis: &redis::aio::ConnectionManager,
    message: &tgbotapi::Message,
    matches: &[fuzzysearch::File],
) -> Result<bool, Error> {
    let group_id = match &message.media_group_id {
        Some(id) => id,
        _ => return Ok(false),
    };

    let key = format!("group-sources:{}", group_id);

    let mut urls: Vec<_> = matches.iter().map(|m| m.url()).collect();
    urls.sort();
    urls.dedup();
    let source_count = urls.len();

    tracing::trace!(%group_id, "adding new sources: {:?}", urls);

    let mut redis = redis.clone();
    let added_links: usize = redis.sadd(&key, urls).await?;
    redis.expire(&key, 300).await?;

    tracing::debug!(
        source_count,
        added_links,
        "determined existing and new source links"
    );

    Ok(source_count > added_links)
}

/// Store if bot has edit permissions in channel.
#[tracing::instrument(skip(cx, message))]
async fn store_channel_edit(cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
    // Check if we already have a saved value for the channel. If we do, it's
    // probably correct and we don't need to update it. Permissions are updated
    // through Telegram updates normally.
    if models::GroupConfig::get::<_, _, bool>(
        &cx.pool,
        models::GroupConfigKey::CanEditChannel,
        &message.chat,
    )
    .await?
    .is_some()
    {
        tracing::trace!("already knew if bot had edit permissions in channel");
        return Ok(());
    }

    let chat_member = cx
        .bot
        .make_request(&GetChatMember {
            chat_id: message.chat_id(),
            user_id: cx.bot_user.id,
        })
        .await?;

    let can_edit_messages = chat_member.can_edit_messages.unwrap_or(false);
    tracing::debug!(
        can_edit_messages,
        "updated if bot can edit messages in channel"
    );

    models::GroupConfig::set(
        &cx.pool,
        models::GroupConfigKey::CanEditChannel,
        &message.chat,
        can_edit_messages,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    async fn get_redis() -> redis::aio::ConnectionManager {
        let redis_client =
            redis::Client::open(std::env::var("REDIS_URL").expect("Missing REDIS_URL")).unwrap();
        redis::aio::ConnectionManager::new(redis_client)
            .await
            .expect("unable to open Redis connection")
    }

    #[tokio::test]
    #[ignore]
    async fn test_already_had_source() {
        let _ = tracing_subscriber::fmt::try_init();

        use super::already_had_source;

        let mut conn = get_redis().await;
        let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();

        let site_info = Some(fuzzysearch::SiteInfo::FurAffinity(
            fuzzysearch::FurAffinityFile { file_id: 123 },
        ));

        let message = tgbotapi::Message {
            media_group_id: Some("test-group".to_string()),
            ..Default::default()
        };

        let sources = vec![fuzzysearch::File {
            site_id: 123,
            site_info: site_info.clone(),
            ..Default::default()
        }];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "filtering with no results should have no status flag"
        );

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            true,
            "filtering with same media group id and source should flag completed"
        );

        let sources = vec![fuzzysearch::File {
            site_id: 456,
            site_info: site_info.clone(),
            ..Default::default()
        }];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "filtering same group with new source should have no status flag"
        );

        let message = tgbotapi::Message {
            media_group_id: Some("test-group-2".to_string()),
            ..Default::default()
        };

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            false,
            "different group should not be affected by other group sources"
        );

        let sources = vec![
            fuzzysearch::File {
                site_id: 456,
                site_info: site_info.clone(),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 789,
                site_info: site_info.clone(),
                ..Default::default()
            },
        ];

        let resp = already_had_source(&conn, &message, &sources).await;
        assert!(resp.is_ok(), "filtering should not cause an error");
        assert_eq!(
            resp.unwrap(),
            true,
            "adding a new with an old source should set a completed flag"
        );
    }
}
