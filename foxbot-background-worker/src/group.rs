use crate::*;
use anyhow::Context;
use foxbot_models::{GroupConfig, GroupConfigKey};
use tgbotapi::requests::GetChat;

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
            "could not update bot knowledge of chat linked channel: {:?}",
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
    let mut matches = match_image(
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
    let sites = handler.sites.lock().await;

    if wanted_matches
        .iter()
        .any(|m| link_was_seen(&sites, &links, &m.url()))
    {
        tracing::debug!("group message already contained valid links");
        return Ok(());
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

/// Check if group is linked to a channel.
#[tracing::instrument(skip(handler, message))]
async fn store_linked_chat(handler: &Handler, message: &tgbotapi::Message) -> anyhow::Result<()> {
    if GroupConfig::get::<Option<i64>>(
        &handler.conn,
        message.chat.id,
        GroupConfigKey::HasLinkedChat,
    )
    .await
    .context("could not check if chat had known linked channel")?
    .is_some()
    {
        tracing::trace!("already knew if chat had linked channel");
        return Ok(());
    }

    let chat = handler
        .telegram
        .make_request(&GetChat {
            chat_id: message.chat_id(),
        })
        .await
        .context("could not get chat information")?;

    if let Some(linked_chat_id) = chat.linked_chat_id {
        tracing::debug!(linked_chat_id, "discovered chat had linked channel");
    } else {
        tracing::debug!("chat did not have linked channel");
    }

    GroupConfig::set(
        &handler.conn,
        GroupConfigKey::HasLinkedChat,
        message.chat.id,
        chat.linked_chat_id,
    )
    .await
    .context("could not save linked chat information")?;

    Ok(())
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
