use std::sync::Arc;

use fluent_bundle::FluentArgs;

use crate::{
    execute::{telegram::Context, telegram_jobs::HashNotifyJob},
    models, utils, Error,
};

use super::NewHashJob;

#[tracing::instrument(skip(cx, job), fields(job_id = job.id()))]
pub async fn process_hash_new(
    cx: Arc<Context>,
    job: faktory::Job,
    hash_job: NewHashJob,
) -> Result<(), Error> {
    let hash = i64::from_be_bytes(hash_job.hash);

    let subscriptions = models::Subscription::search(&cx.pool, hash).await?;
    if subscriptions.is_empty() {
        tracing::trace!("got hash with no subscriptions");
        return Ok(());
    }

    tracing::debug!("found hash with subscriptions, loading full information");

    let matches = utils::lookup_single_hash(&cx.fuzzysearch, hash, Some(3), true).await?;
    if matches.is_empty() {
        tracing::warn!("got hash notification but found no matches");
        return Ok(());
    }

    let bundle = cx.get_fluent_bundle(None).await;

    let text = if matches.len() == 1 {
        let file = matches.first().unwrap();

        let mut args = FluentArgs::new();
        args.set("link", file.url());

        utils::get_message(&bundle, "subscribe-found-single", Some(args))
    } else {
        let mut buf = String::new();

        buf.push_str(&utils::get_message(
            &bundle,
            "subscribe-found-multiple",
            None,
        ));
        buf.push('\n');

        for result in matches {
            let mut args = FluentArgs::new();
            args.set("link", result.url());

            let message = utils::get_message(&bundle, "subscribe-found-multiple-item", Some(args));

            buf.push_str(&message);
            buf.push('\n');
        }

        buf
    };

    for sub in subscriptions {
        let telegram_id = match sub.telegram_id {
            Some(id) => id,
            None => continue,
        };

        let job = HashNotifyJob {
            telegram_id,
            text: text.clone(),
            searched_hash: sub.hash.to_be_bytes(),
            message_id: sub.message_id,
            photo_id: sub.photo_id,
        };

        cx.faktory.enqueue_job(job, None).await?;
    }

    Ok(())
}

#[tracing::instrument(skip(cx, job), fields(job_id = job.id()))]
pub async fn process_hash_notify(
    cx: Arc<Context>,
    job: faktory::Job,
    notify: HashNotifyJob,
) -> Result<(), Error> {
    use tgbotapi::requests::{SendMessage, SendPhoto};

    let mut was_sent = false;

    if let Some(photo_id) = notify.photo_id {
        let send_photo = SendPhoto {
            photo: tgbotapi::FileType::FileID(photo_id),
            chat_id: notify.telegram_id.into(),
            reply_to_message_id: notify.message_id,
            allow_sending_without_reply: Some(true),
            caption: Some(notify.text.clone()),
            ..Default::default()
        };

        if cx.bot.make_request(&send_photo).await.is_ok() {
            was_sent = true;
        }
    }

    if !was_sent {
        let send_message = SendMessage {
            chat_id: notify.telegram_id.into(),
            reply_to_message_id: notify.message_id,
            text: notify.text,
            allow_sending_without_reply: Some(true),
            ..Default::default()
        };
        cx.bot.make_request(&send_message).await?;
    }

    models::Subscription::remove(
        &cx.pool,
        models::User::Telegram(notify.telegram_id),
        i64::from_be_bytes(notify.searched_hash),
    )
    .await?;

    Ok(())
}
