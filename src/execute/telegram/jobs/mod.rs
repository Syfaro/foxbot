use chrono::TimeZone;
use redis::AsyncCommands;

use crate::{models, Error};

use super::Context;

pub(super) mod channel;
pub(super) mod group;
pub(super) mod subscribe;

const MAX_SOURCE_DISTANCE: u64 = 3;
const NOISY_SOURCE_COUNT: usize = 4;

#[derive(serde::Deserialize, serde::Serialize)]
struct GroupSource {
    chat_id: String,
    reply_to_message_id: i32,
    text: String,
}

/// Set that chat needs additional time before another message can be sent.
#[tracing::instrument(skip(redis))]
async fn needs_more_time(
    redis: &redis::aio::ConnectionManager,
    chat_id: &str,
    at: chrono::DateTime<chrono::Utc>,
) {
    let mut redis = redis.clone();

    let now = chrono::Utc::now();
    let seconds = (at - now).num_seconds();

    if seconds <= 0 {
        tracing::warn!(seconds, "needed time already happened");
    }

    let key = format!("retry-at:{}", chat_id);
    if let Err(err) = redis
        .set_ex::<_, _, ()>(&key, at.timestamp(), seconds as usize)
        .await
    {
        tracing::error!("unable to set retry-at: {:?}", err);
    }
}

/// Check if a chat needs more time before a message can be sent.
#[tracing::instrument(skip(redis))]
async fn check_more_time(
    redis: &redis::aio::ConnectionManager,
    chat_id: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let mut redis = redis.clone();

    let key = format!("retry-at:{}", chat_id);
    match redis.get::<_, Option<i64>>(&key).await {
        Ok(Some(timestamp)) => {
            let after = chrono::Utc.timestamp(timestamp, 0);
            if after <= chrono::Utc::now() {
                tracing::trace!("retry-at was in past, ignoring");
                None
            } else {
                Some(after)
            }
        }
        Ok(None) => None,
        Err(err) => {
            tracing::error!("unable to get retry-at: {:?}", err);

            None
        }
    }
}

/// Check if one chat is linked to another chat.
///
/// Returns if the message was sent to a chat linked to a different chat.
#[tracing::instrument(skip(cx, message))]
async fn store_linked_chat(cx: &Context, message: &tgbotapi::Message) -> Result<bool, Error> {
    if let Some(linked_chat) = models::GroupConfig::get::<_, _, Option<i64>>(
        &cx.pool,
        models::GroupConfigKey::HasLinkedChat,
        &message.chat,
    )
    .await?
    {
        tracing::trace!("already knew if had linked chat");
        return Ok(linked_chat.is_some());
    }

    let chat = cx
        .bot
        .make_request(&tgbotapi::requests::GetChat {
            chat_id: message.chat_id(),
        })
        .await?;

    if let Some(linked_chat_id) = chat.linked_chat_id {
        tracing::debug!(linked_chat_id, "discovered linked chat");
    } else {
        tracing::debug!("no linked chat found");
    }

    models::GroupConfig::set(
        &cx.pool,
        models::GroupConfigKey::HasLinkedChat,
        &message.chat,
        chat.linked_chat_id,
    )
    .await?;

    Ok(chat.linked_chat_id.is_some())
}

/// Check if the message was a forward from a chat, and if it was, check if we
/// have edit permissions in that channel. If we do, we can skip this message as
/// it will get a source applied automatically.
#[tracing::instrument(skip(cx, message), fields(forward_from_chat_id))]
async fn is_controlled_channel(cx: &Context, message: &tgbotapi::Message) -> Result<bool, Error> {
    // If it wasn't forwarded from a channel, there's no way it's from a
    // controlled channel.
    let forward_from_chat = match &message.forward_from_chat {
        Some(chat) => chat,
        None => return Ok(false),
    };

    tracing::Span::current().record("forward_from_chat_id", &forward_from_chat.id);

    let can_edit = match models::GroupConfig::get::<_, _, bool>(
        &cx.pool,
        models::GroupConfigKey::CanEditChannel,
        models::Chat::Telegram(forward_from_chat.id),
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
