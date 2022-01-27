use chrono::TimeZone;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::{
    models,
    services::faktory::{BotJob, JobQueue},
    Error,
};

use super::Context;

pub(super) mod channel;
pub(super) mod group;
pub(super) mod subscribe;

const MAX_SOURCE_DISTANCE: u64 = 3;
const NOISY_SOURCE_COUNT: usize = 4;

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

pub enum TelegramJobQueue {
    Default,
    StandardPriority,
    HighPriority,
}

impl JobQueue for TelegramJobQueue {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "foxbot_telegram_default",
            Self::HighPriority => "foxbot_telegram_high",
            Self::StandardPriority => "foxbot_telegram_standard",
        }
    }

    fn priority_order() -> Vec<&'static str> {
        vec![
            Self::Default.as_str(),
            Self::HighPriority.as_str(),
            Self::StandardPriority.as_str(),
        ]
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TwitterAccountAddedJob {
    pub telegram_id: Option<i64>,
    pub twitter_username: String,
}

impl BotJob<TelegramJobQueue> for TwitterAccountAddedJob {
    const NAME: &'static str = "twitter_account_added";

    type JobData = Self;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewHashJob {
    pub hash: [u8; 8],
}

impl BotJob<TelegramJobQueue> for NewHashJob {
    const NAME: &'static str = "hash_new";

    type JobData = Self;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct TelegramIngestJob {
    pub update: serde_json::Value,
}

impl TelegramIngestJob {
    pub fn is_high_priority(&self) -> bool {
        matches!(serde_json::from_value::<tgbotapi::Update>(self.update.clone()), Ok(update) if update.inline_query.is_some())
    }
}

impl BotJob<TelegramJobQueue> for TelegramIngestJob {
    const NAME: &'static str = "ingest_telegram";

    type JobData = tgbotapi::Update;

    fn queue(&self) -> TelegramJobQueue {
        if self.is_high_priority() {
            TelegramJobQueue::HighPriority
        } else {
            TelegramJobQueue::StandardPriority
        }
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![self.update])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum CoconutEventJob {
    Progress {
        display_name: String,
        progress: String,
    },
    Completed {
        display_name: String,
        video_url: String,
        thumb_url: String,
        video_size: i32,
    },
    Failed {
        display_name: String,
    },
}

impl BotJob<TelegramJobQueue> for CoconutEventJob {
    const NAME: &'static str = "coconut_progress";

    type JobData = Self;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct ChannelUpdateJob<'a> {
    pub message: &'a tgbotapi::Message,
}

impl BotJob<TelegramJobQueue> for ChannelUpdateJob<'_> {
    const NAME: &'static str = "channel_update";

    type JobData = tgbotapi::Message;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.message)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct GroupPhotoJob<'a> {
    pub message: &'a tgbotapi::Message,
}

impl BotJob<TelegramJobQueue> for GroupPhotoJob<'_> {
    const NAME: &'static str = "group_photo";

    type JobData = tgbotapi::Message;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.message)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageEdit {
    pub chat_id: String,
    pub message_id: i32,
    pub media_group_id: Option<String>,
    pub firsts: Vec<(crate::models::Sites, String)>,
}

#[derive(Debug)]
pub struct ChannelEditJob {
    pub message_edit: MessageEdit,
    pub has_linked_chat: bool,
}

impl BotJob<TelegramJobQueue> for ChannelEditJob {
    const NAME: &'static str = "channel_edit";

    type JobData = (MessageEdit, bool);

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.message_edit)?;
        Ok(vec![value, serde_json::Value::Bool(self.has_linked_chat)])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        let message_edit = serde_json::from_value(args.remove(0))?;
        let has_linked_chat: bool = serde_json::from_value(args.remove(0))?;

        Ok((message_edit, has_linked_chat))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HashNotifyJob {
    pub telegram_id: i64,
    pub text: String,
    pub message_id: Option<i32>,
    pub photo_id: Option<String>,
    pub searched_hash: [u8; 8],
}

impl BotJob<TelegramJobQueue> for HashNotifyJob {
    const NAME: &'static str = "hash_notify";

    type JobData = Self;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GroupSourceJob {
    pub chat_id: String,
    pub reply_to_message_id: i32,
    pub text: String,
}

impl BotJob<TelegramJobQueue> for GroupSourceJob {
    const NAME: &'static str = "group_source";

    type JobData = Self;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct MediaGroupMessageJob {
    pub message: tgbotapi::Message,
}

impl BotJob<TelegramJobQueue> for MediaGroupMessageJob {
    const NAME: &'static str = "group_mediagroup_message";

    type JobData = tgbotapi::Message;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.message)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct MediaGroupCheckJob {
    pub media_group_id: String,
}

impl BotJob<TelegramJobQueue> for MediaGroupCheckJob {
    const NAME: &'static str = "group_mediagroup_check";

    type JobData = String;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.media_group_id)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct MediaGroupHashJob {
    pub saved_message_id: i32,
}

impl BotJob<TelegramJobQueue> for MediaGroupHashJob {
    const NAME: &'static str = "group_mediagroup_hash";

    type JobData = i32;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![serde_json::Value::Number(
            self.saved_message_id.into(),
        )])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug)]
pub struct MediaGroupPruneJob {
    pub media_group_id: String,
}

impl BotJob<TelegramJobQueue> for MediaGroupPruneJob {
    const NAME: &'static str = "group_mediagroup_prune";

    type JobData = String;

    fn queue(&self) -> TelegramJobQueue {
        TelegramJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self.media_group_id)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}
