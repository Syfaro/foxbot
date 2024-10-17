use bigdecimal::BigDecimal;
use lazy_static::lazy_static;
use prometheus::{register_counter_vec, CounterVec};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, PgExecutor, PgPool};
use tgbotapi::Message;

use crate::Error;

lazy_static! {
    static ref CACHE_REQUESTS: CounterVec = register_counter_vec!(
        "foxbot_cache_requests_total",
        "Number of file cache hits and misses",
        &["result"]
    )
    .unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum User {
    Telegram(i64),
    Discord(BigDecimal),
}

impl User {
    #[allow(clippy::manual_map)]
    pub fn from_one(telegram_id: Option<i64>, discord_id: Option<BigDecimal>) -> Option<Self> {
        if let Some(id) = telegram_id {
            Some(Self::Telegram(id))
        } else if let Some(id) = discord_id {
            Some(Self::Discord(id))
        } else {
            None
        }
    }

    pub fn telegram_id(&self) -> Option<i64> {
        match self {
            Self::Telegram(id) => Some(*id),
            Self::Discord(_) => None,
        }
    }

    fn discord_id(&self) -> Option<BigDecimal> {
        match self {
            Self::Discord(id) => Some(id.to_owned()),
            Self::Telegram(_) => None,
        }
    }

    pub fn unleash_context(&self) -> foxlib::flags::Context {
        let user_id = match self {
            Self::Telegram(id) => id.to_string(),
            Self::Discord(id) => id.to_string(),
        };

        foxlib::flags::Context {
            user_id: Some(user_id),
            properties: [(
                "appVersion".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }
}

impl std::fmt::Display for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Telegram(id) => write!(f, "Telegram-{}", id),
            Self::Discord(id) => write!(f, "Discord-{}", id),
        }
    }
}

impl From<&User> for User {
    fn from(user: &User) -> Self {
        user.to_owned()
    }
}

impl From<&tgbotapi::User> for User {
    fn from(user: &tgbotapi::User) -> Self {
        Self::Telegram(user.id)
    }
}

impl From<twilight_model::id::Id<twilight_model::id::marker::UserMarker>> for User {
    fn from(user_id: twilight_model::id::Id<twilight_model::id::marker::UserMarker>) -> Self {
        let id: u64 = user_id.get();
        Self::Discord(id.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sites {
    FurAffinity,
    #[serde(rename = "e621")]
    E621,
    Twitter,
    Weasyl,
}

impl Sites {
    pub fn len() -> usize {
        4
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FurAffinity => "FurAffinity",
            Self::E621 => "e621",
            Self::Twitter => "Twitter",
            Self::Weasyl => "Weasyl",
        }
    }

    pub fn default_order() -> Vec<Self> {
        vec![Self::FurAffinity, Self::Weasyl, Self::E621, Self::Twitter]
    }
}

impl std::str::FromStr for Sites {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "FurAffinity" => Ok(Self::FurAffinity),
            "e621" => Ok(Self::E621),
            "Twitter" => Ok(Self::Twitter),
            "Weasyl" => Ok(Self::Weasyl),
            _ => Err(Error::bot("unknown site")),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Chat {
    Telegram(i64),
}

impl Chat {
    pub fn telegram_id(&self) -> Option<i64> {
        match self {
            Self::Telegram(id) => Some(*id),
        }
    }

    pub fn discord_id(&self) -> Option<BigDecimal> {
        match self {
            Self::Telegram(_id) => None,
        }
    }
}

impl From<&Chat> for Chat {
    fn from(chat: &Chat) -> Self {
        chat.to_owned()
    }
}

impl From<&tgbotapi::Chat> for Chat {
    fn from(chat: &tgbotapi::Chat) -> Self {
        Self::Telegram(chat.id)
    }
}

pub enum UserConfigKey {
    SiteSortOrder,
    InlineHistory,
}

impl UserConfigKey {
    fn as_str(&self) -> &'static str {
        match self {
            Self::SiteSortOrder => "site-sort-order",
            Self::InlineHistory => "inline-history",
        }
    }
}

pub struct UserConfig;

impl UserConfig {
    pub async fn get<'a, E, U, D>(
        executor: E,
        key: UserConfigKey,
        user: U,
    ) -> Result<Option<D>, Error>
    where
        E: PgExecutor<'a>,
        D: serde::de::DeserializeOwned,
        U: Into<User>,
    {
        let user = user.into();

        let config = sqlx::query_file_scalar!(
            "queries/user_config/get.sql",
            user.telegram_id(),
            user.discord_id(),
            key.as_str()
        )
        .fetch_optional(executor)
        .await?
        .and_then(|config| serde_json::from_value(config).ok());

        Ok(config)
    }

    pub async fn set<'a, E, U, D>(
        executor: E,
        key: UserConfigKey,
        user: U,
        data: D,
    ) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        U: Into<User>,
        D: serde::Serialize,
    {
        let user = user.into();
        let value = serde_json::to_value(data)?;

        sqlx::query_file!(
            "queries/user_config/set.sql",
            user.telegram_id(),
            user.discord_id(),
            key.as_str(),
            value
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn delete<'a, E, U>(executor: E, key: UserConfigKey, user: U) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        U: Into<User>,
    {
        let user = user.into();

        sqlx::query_file!(
            "queries/user_config/delete.sql",
            user.telegram_id(),
            user.discord_id(),
            key.as_str()
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub enum GroupConfigKey {
    GroupAdd,
    GroupNoPreviews,
    GroupNoAlbums,
    HasDeletePermission,
    CanEditChannel,
    HasLinkedChat,
    ChannelCaption,
    Nsfw,
}

impl GroupConfigKey {
    fn as_str(&self) -> &'static str {
        match self {
            Self::GroupAdd => "group_add",
            Self::GroupNoPreviews => "group_no_previews",
            Self::GroupNoAlbums => "group_no_albums",
            Self::HasDeletePermission => "has_delete_permission",
            Self::CanEditChannel => "can_edit_channel",
            Self::HasLinkedChat => "has_linked_chat",
            Self::ChannelCaption => "channel_caption",
            Self::Nsfw => "nsfw",
        }
    }
}

pub struct GroupConfig;

impl GroupConfig {
    pub async fn get<'a, E, C, D>(
        executor: E,
        key: GroupConfigKey,
        chat: C,
    ) -> Result<Option<D>, Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
        D: serde::de::DeserializeOwned,
    {
        let chat = chat.into();

        let config = sqlx::query_file_scalar!(
            "queries/group_config/get.sql",
            chat.telegram_id(),
            chat.discord_id(),
            key.as_str()
        )
        .fetch_optional(executor)
        .await?
        .and_then(|config| serde_json::from_value(config).ok());

        Ok(config)
    }

    pub async fn get_max_age<'a, E, C, D>(
        executor: E,
        key: GroupConfigKey,
        chat: C,
        max_age: chrono::Duration,
    ) -> Result<Option<D>, Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
        D: serde::de::DeserializeOwned,
    {
        let chat = chat.into();

        let oldest_time = (chrono::Utc::now() - max_age).naive_utc();

        let config = sqlx::query_file_scalar!(
            "queries/group_config/get_max_age.sql",
            chat.telegram_id(),
            chat.discord_id(),
            key.as_str(),
            oldest_time,
        )
        .fetch_optional(executor)
        .await?
        .and_then(|config| serde_json::from_value(config).ok());

        Ok(config)
    }

    pub async fn set<'a, E, C, D>(
        executor: E,
        key: GroupConfigKey,
        chat: C,
        data: D,
    ) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
        D: serde::Serialize,
    {
        let chat = chat.into();
        let value = serde_json::to_value(data)?;

        sqlx::query_file!(
            "queries/group_config/set.sql",
            chat.telegram_id(),
            chat.discord_id(),
            key.as_str(),
            value
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub struct Subscription {
    pub telegram_id: Option<i64>,
    pub hash: i64,
    pub message_id: Option<i32>,
    pub photo_id: Option<String>,
}

impl Subscription {
    pub async fn add<'a, E: PgExecutor<'a>, U: Into<User>>(
        executor: E,
        user: U,
        hash: i64,
        message_id: Option<i32>,
        photo_id: Option<&str>,
    ) -> Result<(), Error> {
        let user = user.into();

        sqlx::query_file!(
            "queries/subscription/add.sql",
            user.telegram_id(),
            user.discord_id(),
            hash,
            message_id,
            photo_id
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn remove<'a, E, U>(executor: E, user: U, hash: i64) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        U: Into<User>,
    {
        let user = user.into();

        sqlx::query_file!(
            "queries/subscription/remove.sql",
            user.telegram_id(),
            user.discord_id(),
            hash
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn search<'a, E: PgExecutor<'a>>(executor: E, hash: i64) -> Result<Vec<Self>, Error> {
        let subscriptions = sqlx::query_file_as!(Self, "queries/subscription/search.sql", hash)
            .fetch_all(executor)
            .await?;

        Ok(subscriptions)
    }
}

pub struct TwitterAuth {
    pub id: i32,
    pub telegram_id: Option<i64>,
    pub discord_id: Option<BigDecimal>,
    pub request_key: String,
    pub request_secret: String,
}

impl TwitterAuth {
    pub async fn get_request<'a, E: PgExecutor<'a>>(
        executor: E,
        request_key: &str,
    ) -> Result<Option<Self>, Error> {
        let auth = sqlx::query_file_as!(Self, "queries/twitter_auth/get_request.sql", request_key)
            .fetch_optional(executor)
            .await?;

        Ok(auth)
    }

    pub async fn set_request<U>(
        pool: &PgPool,
        user: U,
        request_key: &str,
        request_secret: &str,
    ) -> Result<(), Error>
    where
        U: Into<User>,
    {
        let user = user.into();

        let mut tx = pool.begin().await?;

        sqlx::query_file!(
            "queries/twitter_auth/delete.sql",
            user.telegram_id(),
            user.discord_id()
        )
        .execute(&mut tx)
        .await?;

        sqlx::query_file!(
            "queries/twitter_auth/set_request.sql",
            user.telegram_id(),
            user.discord_id(),
            request_key,
            request_secret
        )
        .execute(&mut tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }
}

pub struct TwitterAccount {
    pub id: i32,
    pub consumer_key: String,
    pub consumer_secret: String,
}

impl TwitterAccount {
    pub async fn get<'a, E, U>(executor: E, user: U) -> Result<Option<Self>, Error>
    where
        E: PgExecutor<'a>,
        U: Into<User>,
    {
        let user = user.into();

        let account = sqlx::query_file_as!(
            Self,
            "queries/twitter_account/get.sql",
            user.telegram_id(),
            user.discord_id()
        )
        .fetch_optional(executor)
        .await?;

        Ok(account)
    }

    pub async fn save_authorization<U: Into<User>>(
        pool: &sqlx::PgPool,
        user: U,
        key_pair: egg_mode::KeyPair,
    ) -> Result<Self, Error> {
        let user = user.into();

        let mut tx = pool.begin().await?;

        sqlx::query_file!(
            "queries/twitter_account/delete.sql",
            user.telegram_id(),
            user.discord_id(),
        )
        .execute(&mut tx)
        .await?;

        sqlx::query_file!(
            "queries/twitter_auth/delete.sql",
            user.telegram_id(),
            user.discord_id()
        )
        .execute(&mut tx)
        .await?;

        let account = sqlx::query_file_as!(
            Self,
            "queries/twitter_account/insert.sql",
            user.telegram_id(),
            user.discord_id(),
            &key_pair.key,
            &key_pair.secret,
        )
        .fetch_one(&mut tx)
        .await?;

        tx.commit().await?;

        Ok(account)
    }

    pub async fn delete<'a, E, U>(executor: E, user: U) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        U: Into<User>,
    {
        let user = user.into();

        sqlx::query_file!(
            "queries/twitter_account/delete.sql",
            user.telegram_id(),
            user.discord_id()
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub struct MediaGroup {
    pub id: i32,
    pub media_group_id: String,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub message: Json<Message>,
    pub sources: Option<Json<Vec<fuzzysearch::File>>>,
}

impl MediaGroup {
    pub async fn add_message<'a, E: PgExecutor<'a>>(
        executor: E,
        message: &tgbotapi::Message,
    ) -> Result<i32, Error> {
        let media_group_id = message
            .media_group_id
            .as_ref()
            .ok_or_else(|| Error::missing("media group id"))?;

        let id = sqlx::query_file_scalar!(
            "queries/media_group/add_message.sql",
            media_group_id,
            chrono::Utc::now(),
            serde_json::to_value(message)?
        )
        .fetch_one(executor)
        .await?;

        Ok(id)
    }

    pub async fn last_message<'a, E: PgExecutor<'a>>(
        executor: E,
        media_group_id: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let last_message =
            sqlx::query_file_scalar!("queries/media_group/last_message.sql", media_group_id)
                .fetch_optional(executor)
                .await?;

        Ok(last_message.flatten())
    }

    pub async fn get_message<'a, E: PgExecutor<'a>>(
        executor: E,
        id: i32,
    ) -> Result<Option<Self>, Error> {
        let message = sqlx::query_file_as!(Self, "queries/media_group/get_message.sql", id)
            .fetch_optional(executor)
            .await?;

        Ok(message)
    }

    pub async fn get_messages<'a, E: PgExecutor<'a>>(
        executor: E,
        media_group_id: &str,
    ) -> Result<Vec<Self>, Error> {
        let messages =
            sqlx::query_file_as!(Self, "queries/media_group/get_messages.sql", media_group_id)
                .fetch_all(executor)
                .await?;

        Ok(messages)
    }

    pub async fn set_message_sources<'a, E: PgExecutor<'a>>(
        executor: E,
        id: i32,
        sources: Vec<fuzzysearch::File>,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/media_group/set_message_sources.sql",
            id,
            serde_json::to_value(sources)?
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn consume_messages<'a, E: PgExecutor<'a>>(
        executor: E,
        media_group_id: &str,
    ) -> Result<Vec<Self>, Error> {
        let messages = sqlx::query_file_as!(
            Self,
            "queries/media_group/consume_messages.sql",
            media_group_id
        )
        .fetch_all(executor)
        .await?;

        Ok(messages)
    }

    pub async fn sending_message<'a, E: PgExecutor<'a>>(
        executor: E,
        media_group_id: &str,
    ) -> Result<bool, Error> {
        let returned_id =
            sqlx::query_file_scalar!("queries/media_group/sending_message.sql", media_group_id)
                .fetch_optional(executor)
                .await?;

        Ok(returned_id.is_some())
    }

    pub async fn purge(pool: &PgPool, media_group_id: &str) -> Result<(), Error> {
        let mut tx = pool.begin().await?;

        sqlx::query_file!("queries/media_group/purge_media_group.sql", media_group_id)
            .execute(&mut tx)
            .await?;

        sqlx::query_file!(
            "queries/media_group/purge_media_group_sent.sql",
            media_group_id
        )
        .execute(&mut tx)
        .await?;

        Ok(())
    }
}

pub struct Video {
    pub id: i32,
    pub processed: bool,
    pub source: String,
    pub url: String,
    pub mp4_url: Option<String>,
    pub job_id: Option<String>,
    pub display_name: String,
    pub thumb_url: Option<String>,
    pub display_url: String,
    pub created_at: chrono::NaiveDateTime,
    pub file_size: Option<i64>,
    pub height: Option<i32>,
    pub width: Option<i32>,
    pub duration: Option<i32>,
}

impl Video {
    pub async fn lookup_by_display_name<'a, E: PgExecutor<'a>>(
        executor: E,
        display_name: &str,
    ) -> Result<Option<Self>, Error> {
        let video = sqlx::query_file_as!(
            Self,
            "queries/video/lookup_by_display_name.sql",
            display_name
        )
        .fetch_optional(executor)
        .await?;

        Ok(video)
    }

    pub async fn lookup_by_url_id<'a, E: PgExecutor<'a>>(
        executor: E,
        url_id: &str,
    ) -> Result<Option<Self>, Error> {
        let video = sqlx::query_file_as!(Self, "queries/video/lookup_by_url_id.sql", url_id)
            .fetch_optional(executor)
            .await?;

        Ok(video)
    }

    pub async fn insert_media<'a, E: PgExecutor<'a>>(
        executor: E,
        url_id: &str,
        media_url: &str,
        display_url: &str,
        display_name: &str,
    ) -> Result<String, Error> {
        let display_name = sqlx::query_file_scalar!(
            "queries/video/insert_media.sql",
            url_id,
            media_url,
            display_url,
            display_name
        )
        .fetch_one(executor)
        .await?;

        Ok(display_name)
    }

    pub async fn set_job_id<'a, E: PgExecutor<'a>>(
        executor: E,
        id: i32,
        job_id: &str,
    ) -> Result<(), Error> {
        sqlx::query_file!("queries/video/set_job_id.sql", id, job_id)
            .execute(executor)
            .await?;

        Ok(())
    }

    pub async fn set_processed_url<'a, E: PgExecutor<'a>>(
        executor: E,
        id: i32,
        mp4_url: &str,
        thumb_url: &str,
        file_size: i64,
        height: i32,
        width: i32,
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/video/set_processed_url.sql",
            id,
            mp4_url,
            thumb_url,
            file_size,
            height,
            width
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn add_message_id<'a, E, C>(
        executor: E,
        id: i32,
        chat: C,
        message_id: i32,
    ) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
    {
        let chat = chat.into();

        sqlx::query_file!(
            "queries/video/add_message_id.sql",
            id,
            chat.telegram_id(),
            chat.discord_id(),
            message_id
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn associated_messages<'a, E: PgExecutor<'a>>(
        executor: E,
        id: i32,
    ) -> Result<Vec<(i64, i32)>, Error> {
        let ids = sqlx::query_file!("queries/video/associated_messages.sql", id)
            .map(|row| (row.chat_id, row.message_id))
            .fetch_all(executor)
            .await?;

        Ok(ids)
    }
}

pub struct Permission;

impl Permission {
    pub async fn add_change<'a, E: PgExecutor<'a>>(
        executor: E,
        my_chat_member: &tgbotapi::ChatMemberUpdated,
    ) -> Result<(), Error> {
        let data = serde_json::to_value(&my_chat_member.new_chat_member)?;

        sqlx::query_file!(
            "queries/permission/add_change.sql",
            my_chat_member.chat.id,
            my_chat_member.date,
            data
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub struct ChatAdmin;

impl ChatAdmin {
    pub async fn update_chat<'a, E, C, U>(
        executor: E,
        chat: C,
        user: U,
        status: &tgbotapi::ChatMemberStatus,
        date: Option<i32>,
    ) -> Result<bool, Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
        U: Into<User>,
    {
        use tgbotapi::ChatMemberStatus::*;

        let chat = chat.into();
        let user = user.into();

        let is_admin = matches!(status, Administrator | Creator);

        let date = date
            .map(|date| date as i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp());

        sqlx::query_file!(
            "queries/chat_admin/update_chat.sql",
            user.telegram_id(),
            user.discord_id(),
            chat.telegram_id(),
            is_admin,
            date
        )
        .execute(executor)
        .await?;

        Ok(is_admin)
    }

    pub async fn is_admin<C, U>(pool: &PgPool, chat: C, user: U) -> Result<Option<bool>, Error>
    where
        C: Into<Chat>,
        U: Into<User>,
    {
        let user = user.into();
        let chat = chat.into();

        let is_admin = sqlx::query_file_scalar!(
            "queries/chat_admin/is_admin.sql",
            user.telegram_id(),
            user.discord_id(),
            chat.telegram_id()
        )
        .fetch_optional(pool)
        .await?;

        Ok(is_admin)
    }

    pub async fn flush<'a, E, C>(executor: E, chat: C, bot_user_id: i64) -> Result<(), Error>
    where
        E: PgExecutor<'a>,
        C: Into<Chat>,
    {
        let chat = chat.into();

        sqlx::query_file!(
            "queries/chat_admin/flush.sql",
            chat.telegram_id(),
            bot_user_id
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub struct CachedPost {
    pub id: i32,
    pub post_url: String,
    pub thumb: bool,
    pub cdn_url: String,
    pub dimensions: (u32, u32),
}

impl CachedPost {
    pub async fn get<'a, E: PgExecutor<'a>>(
        executor: E,
        post_url: &str,
        thumb: bool,
    ) -> Result<Option<Self>, Error> {
        let post = sqlx::query_file!("queries/cached_post/get.sql", post_url, thumb)
            .map(|row| CachedPost {
                id: row.id,
                post_url: row.post_url,
                thumb: row.thumb,
                cdn_url: row.cdn_url,
                dimensions: (row.width as u32, row.height as u32),
            })
            .fetch_optional(executor)
            .await?;

        Ok(post)
    }

    pub async fn save<'a, E: PgExecutor<'a>>(
        executor: E,
        post_url: &str,
        cdn_url: &str,
        thumb: bool,
        dimensions: (u32, u32),
    ) -> Result<(), Error> {
        sqlx::query_file!(
            "queries/cached_post/save.sql",
            post_url,
            thumb,
            cdn_url,
            dimensions.0 as i64,
            dimensions.1 as i64
        )
        .execute(executor)
        .await?;

        Ok(())
    }
}

pub struct FileCache;

impl FileCache {
    pub async fn get(
        redis: &redis::aio::ConnectionManager,
        file_id: &str,
    ) -> Result<Option<i64>, Error> {
        let mut redis = redis.clone();

        let result = redis.get::<_, Option<i64>>(file_id).await?;

        let status = if result.is_some() { "hit" } else { "miss" };
        CACHE_REQUESTS.with_label_values(&[status]).inc();

        Ok(result)
    }

    pub async fn set(
        redis: &redis::aio::ConnectionManager,
        file_id: &str,
        hash: i64,
    ) -> Result<(), Error> {
        let mut redis = redis.clone();

        redis.set_ex(file_id, hash, 60 * 60 * 24 * 7).await?;

        Ok(())
    }
}

pub struct Manage;

#[derive(Serialize)]
pub struct SettingEntry {
    pub name: String,
    pub value: serde_json::Value,
}

impl SettingEntry {
    pub fn new<N, V>(name: N, value: V) -> Self
    where
        N: ToString,
        V: Into<serde_json::Value>,
    {
        Self {
            name: name.to_string(),
            value: value.into(),
        }
    }
}

impl Manage {
    pub async fn get_user_groups<U: Into<User>>(pool: &PgPool, user: U) -> Result<Vec<i64>, Error> {
        let user = user.into();

        let chat_ids = sqlx::query_file!(
            "queries/manage/get_user_groups.sql",
            user.telegram_id(),
            user.discord_id()
        )
        .map(|row| row.telegram_id)
        .fetch_all(pool)
        .await?;

        Ok(chat_ids)
    }

    pub async fn get_chat_config<C: Into<Chat>>(
        pool: &PgPool,
        chat: C,
    ) -> Result<Vec<(String, serde_json::Value)>, Error> {
        let chat = chat.into();

        let config = sqlx::query_file!(
            "queries/manage/get_chat_config.sql",
            chat.telegram_id(),
            chat.discord_id()
        )
        .map(|row| (row.name, row.value))
        .fetch_all(pool)
        .await?;

        Ok(config)
    }

    pub async fn update_settings<C: Into<Chat>>(
        pool: &PgPool,
        chat: C,
        settings: &[SettingEntry],
    ) -> Result<(), Error> {
        let chat = chat.into();
        let settings = serde_json::to_value(settings)?;

        sqlx::query_file!(
            "queries/manage/update_settings.sql",
            chat.telegram_id(),
            chat.discord_id(),
            settings
        )
        .execute(pool)
        .await?;

        Ok(())
    }
}
