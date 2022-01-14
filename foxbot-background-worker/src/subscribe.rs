use std::sync::Arc;

use crate::*;
use fluent_bundle::FluentArgs;
use foxbot_models::{Subscriptions, User};

#[derive(serde::Serialize, serde::Deserialize)]
struct HashNotify {
    telegram_id: i64,
    text: String,
    message_id: Option<i32>,
    photo_id: Option<String>,
    #[serde(with = "string")]
    searched_hash: i64,
}

mod string {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Display,
        S: Serializer,
    {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id()))]
pub async fn process_hash_new(handler: Arc<Handler>, job: faktory::Job) -> Result<(), Error> {
    let data = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();
    let message: String = serde_json::value::from_value(data)?;
    let hash = message.parse().map_err(|_| Error::MissingData)?;

    let subscriptions = Subscriptions::search_subscriptions(&handler.conn, hash).await?;
    if subscriptions.is_empty() {
        tracing::trace!("got hash with no subscriptions");
        return Ok(());
    }

    tracing::debug!("found hash with subscriptions, loading full information");

    let matches = lookup_single_hash(&handler.fuzzysearch, hash, Some(3)).await?;
    if matches.is_empty() {
        tracing::warn!("got hash notification but found no matches");
        return Ok(());
    }

    let bundle = handler.get_fluent_bundle(None).await;

    let text = if matches.len() == 1 {
        let file = matches.first().unwrap();

        let mut args = FluentArgs::new();
        args.set("link", file.url());

        get_message(&bundle, "subscribe-found-single", Some(args)).unwrap()
    } else {
        let mut buf = String::new();

        buf.push_str(&get_message(&bundle, "subscribe-found-multiple", None).unwrap());
        buf.push('\n');

        for result in matches {
            let mut args = FluentArgs::new();
            args.set("link", result.url());

            let message =
                get_message(&bundle, "subscribe-found-multiple-item", Some(args)).unwrap();

            buf.push_str(&message);
            buf.push('\n');
        }

        buf
    };

    for sub in subscriptions {
        let data = serde_json::to_value(&HashNotify {
            telegram_id: sub.telegram_id,
            text: text.clone(),
            searched_hash: sub.hash,
            message_id: sub.message_id,
            photo_id: sub.photo_id,
        })?;

        let mut job = faktory::Job::new("hash_notify", vec![data]).on_queue("foxbot_background");
        job.custom = get_faktory_custom();

        handler.enqueue(job).await;
    }

    Ok(())
}

#[tracing::instrument(skip(handler, job), fields(job_id = job.id()))]
pub async fn process_hash_notify(handler: Arc<Handler>, job: faktory::Job) -> Result<(), Error> {
    use tgbotapi::requests::{SendMessage, SendPhoto};

    let data = job
        .args()
        .iter()
        .next()
        .ok_or(Error::MissingData)?
        .to_owned();
    let notify: HashNotify = serde_json::value::from_value(data)?;

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

        if handler.telegram.make_request(&send_photo).await.is_ok() {
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
        handler.telegram.make_request(&send_message).await?;
    }

    Subscriptions::remove_subscription(
        &handler.conn,
        User::Telegram(notify.telegram_id),
        notify.searched_hash,
    )
    .await?;

    Ok(())
}
