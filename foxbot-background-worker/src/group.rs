use crate::*;

#[tracing::instrument(skip(handler, job), fields(job_id = job.id()))]
#[deny(clippy::unwrap_used)]
pub async fn process_group_photo(handler: Arc<Handler>, job: faktory::Job) -> Result<(), Error> {
    use foxbot_models::{GroupConfig, GroupConfigKey};

    let data: serde_json::Value = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();

    let message: tgbotapi::Message = serde_json::value::from_value(data)?;
    let photo_sizes = match &message.photo {
        Some(sizes) => sizes,
        _ => return Ok(()),
    };

    tracing::trace!("got enqueued message: {:?}", message);

    match GroupConfig::get(&handler.conn, message.chat.id, GroupConfigKey::GroupAdd).await? {
        Some(val) if val => (),
        _ => return Ok(()),
    }

    let best_photo = find_best_photo(&photo_sizes).unwrap();
    let mut matches = match_image(
        &handler.telegram,
        &handler.conn,
        &handler.fuzzysearch,
        &best_photo,
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
        return Ok(());
    }

    let links = extract_links(&message);
    let sites = handler.sites.lock().await;

    if wanted_matches
        .iter()
        .any(|m| link_was_seen(&sites, &links, &m.url()))
    {
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
                        let rating = get_message(&bundle, rating, None).unwrap();
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

#[tracing::instrument(skip(handler, job), fields(job_id = job.id()))]
#[deny(clippy::unwrap_used)]
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
