use anyhow::Context;
use tgbotapi::Message;

lazy_static::lazy_static! {
    static ref CACHE_REQUESTS: prometheus::CounterVec = prometheus::register_counter_vec!("foxbot_cache_requests_total", "Number of file cache hits and misses", &["result"]).unwrap();
}

/// An error message that is safe to be displayed to the user.
///
/// This should not contain any technical or internal information.
#[derive(thiserror::Error, Debug)]
#[error("{msg}")]
pub struct DisplayableErrorMessage<'a> {
    pub msg: std::borrow::Cow<'a, str>,
    #[source]
    source: anyhow::Error,
}

impl<'a> DisplayableErrorMessage<'a> {
    pub fn new<M, E>(msg: M, err: E) -> Self
    where
        M: Into<std::borrow::Cow<'a, str>>,
        E: Into<anyhow::Error>,
    {
        DisplayableErrorMessage {
            msg: msg.into(),
            source: err.into(),
        }
    }
}

/// Each available site, for configuration usage.
#[derive(Clone, Debug, PartialEq, Hash, Eq, serde::Deserialize)]
pub enum Sites {
    FurAffinity,
    #[serde(rename = "e621")]
    E621,
    Twitter,
    Weasyl,
}

impl serde::Serialize for Sites {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum User {
    Telegram(i64),
    Discord(u64),
}

impl std::fmt::Display for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            User::Telegram(telegram_id) => write!(f, "Telegram-{}", telegram_id),
            User::Discord(discord_id) => write!(f, "Discord-{}", discord_id),
        }
    }
}

impl From<&tgbotapi::User> for User {
    fn from(user: &tgbotapi::User) -> Self {
        User::Telegram(user.id)
    }
}

#[cfg(feature = "discord")]
impl From<twilight_model::id::UserId> for User {
    fn from(user_id: twilight_model::id::UserId) -> Self {
        User::Discord(user_id.0)
    }
}

impl User {
    pub fn telegram_id(&self) -> Option<i64> {
        if let User::Telegram(telegram_id) = self {
            Some(*telegram_id)
        } else {
            None
        }
    }

    pub fn discord_id(&self) -> Option<bigdecimal::BigDecimal> {
        use bigdecimal::FromPrimitive;

        if let User::Discord(discord_id) = self {
            bigdecimal::BigDecimal::from_u64(*discord_id)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Chat {
    Telegram(i64),
    Discord(u64),
}

impl Chat {
    pub fn telegram_id(&self) -> Option<i64> {
        if let Chat::Telegram(telegram_id) = self {
            Some(*telegram_id)
        } else {
            None
        }
    }

    pub fn discord_id(&self) -> Option<bigdecimal::BigDecimal> {
        use bigdecimal::FromPrimitive;

        if let Chat::Discord(discord_id) = self {
            bigdecimal::BigDecimal::from_u64(*discord_id)
        } else {
            None
        }
    }
}

impl From<&tgbotapi::Chat> for Chat {
    fn from(chat: &tgbotapi::Chat) -> Self {
        Chat::Telegram(chat.id)
    }
}

#[derive(Debug)]
pub struct ParseSitesError;

impl std::str::FromStr for Sites {
    type Err = ParseSitesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "FurAffinity" => Ok(Self::FurAffinity),
            "e621" => Ok(Self::E621),
            "Twitter" => Ok(Self::Twitter),
            "Weasyl" => Ok(Self::Weasyl),
            _ => Err(ParseSitesError),
        }
    }
}

impl Sites {
    /// Get the number of known sites.
    pub fn len() -> usize {
        4
    }

    /// Get the user-understandable name of the site.
    pub fn as_str(&self) -> &'static str {
        match *self {
            Self::FurAffinity => "FurAffinity",
            Self::E621 => "e621",
            Self::Twitter => "Twitter",
            Self::Weasyl => "Weasyl",
        }
    }

    /// The bot's default site ordering.
    pub fn default_order() -> Vec<Self> {
        vec![Self::FurAffinity, Self::Weasyl, Self::E621, Self::Twitter]
    }
}

pub struct UserConfig;

pub enum UserConfigKey {
    SiteSortOrder,
}

impl UserConfigKey {
    fn as_str(&self) -> &str {
        match self {
            UserConfigKey::SiteSortOrder => "site-sort-order",
        }
    }
}

impl UserConfig {
    /// Get a configuration value from the user_config table.
    ///
    /// If the value does not exist for a given user, returns None.
    pub async fn get<T: serde::de::DeserializeOwned, U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: UserConfigKey,
        user: U,
    ) -> anyhow::Result<Option<T>> {
        let user = user.into();

        sqlx::query!(
            "SELECT value
            FROM user_config
            WHERE user_config.account_id = lookup_account($1, $2) AND name = $3
            ORDER BY updated_at DESC LIMIT 1",
            user.telegram_id(),
            user.discord_id(),
            key.as_str()
        )
        .fetch_optional(conn)
        .await
        .map(|row| row.map(|row| serde_json::from_value(row.value).unwrap()))
        .context("unable to perform user_config lookup")
    }

    /// Set a configuration value for the user_config table.
    pub async fn set<T: serde::Serialize, U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: UserConfigKey,
        user: U,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(&data)?;
        let user = user.into();

        sqlx::query!(
            "INSERT INTO user_config (account_id, name, value)
            VALUES (lookup_account($1, $2), $3, $4)",
            user.telegram_id(),
            user.discord_id(),
            key.as_str(),
            value
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    /// Delete a config item.
    pub async fn delete(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: UserConfigKey,
        user: User,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM user_config
            WHERE account_id = lookup_account($1, $2) AND name = $3",
            user.telegram_id(),
            user.discord_id(),
            key.as_str()
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct GroupConfig;

pub enum GroupConfigKey {
    GroupAdd,
    GroupNoPreviews,
    GroupNoAlbums,
    HasDeletePermission,
    CanEditChannel,
    HasLinkedChat,
}

impl GroupConfigKey {
    fn as_str(&self) -> &str {
        match self {
            GroupConfigKey::GroupAdd => "group_add",
            GroupConfigKey::GroupNoPreviews => "group_no_previews",
            &GroupConfigKey::GroupNoAlbums => "group_no_albums",
            GroupConfigKey::HasDeletePermission => "has_delete_permission",
            GroupConfigKey::CanEditChannel => "can_edit_channel",
            GroupConfigKey::HasLinkedChat => "has_linked_chat",
        }
    }
}

impl GroupConfig {
    pub async fn get<T: serde::de::DeserializeOwned, C: Into<Chat>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        chat: C,
        name: GroupConfigKey,
    ) -> anyhow::Result<Option<T>> {
        let chat = chat.into();

        sqlx::query!(
            "SELECT value
            FROM group_config
            WHERE group_config.chat_id = lookup_chat($1, $2) AND name = $3
            ORDER BY updated_at DESC LIMIT 1",
            chat.telegram_id(),
            chat.discord_id(),
            name.as_str(),
        )
        .fetch_optional(conn)
        .await
        .map(|row| row.map(|row| serde_json::from_value(row.value).unwrap()))
        .context("unable to perform group_config lookup")
    }

    pub async fn set<T: serde::Serialize, C: Into<Chat>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: GroupConfigKey,
        chat: C,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(data)?;

        let chat = chat.into();

        sqlx::query!(
            "INSERT INTO group_config (chat_id, name, value) VALUES
                (lookup_chat($1, $2), $3, $4)",
            chat.telegram_id(),
            chat.discord_id(),
            key.as_str(),
            value
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

/// A Twitter account, as stored within the database.
#[derive(sqlx::FromRow)]
pub struct TwitterAccount {
    pub consumer_key: String,
    pub consumer_secret: String,
}

#[derive(sqlx::FromRow)]
pub struct TwitterRequest {
    pub account_id: i32,
    pub telegram_id: Option<i64>,
    pub discord_id: Option<u64>,
    pub request_key: String,
    pub request_secret: String,
}

pub struct Twitter;

impl Twitter {
    /// Look up a user's Twitter credentials.
    pub async fn get_account<U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
    ) -> anyhow::Result<Option<TwitterAccount>> {
        let user = user.into();

        let account = sqlx::query_as!(
            TwitterAccount,
            "SELECT consumer_key, consumer_secret
            FROM twitter_account
            WHERE twitter_account.account_id = lookup_account($1, $2)",
            user.telegram_id(),
            user.discord_id(),
        )
        .fetch_optional(conn)
        .await?;

        Ok(account)
    }

    /// Look up a pending request to sign into a Twitter account.
    pub async fn get_request(
        conn: &sqlx::Pool<sqlx::Postgres>,
        request_key: &str,
    ) -> anyhow::Result<Option<TwitterRequest>> {
        use bigdecimal::ToPrimitive;

        let req = sqlx::query!(
            "SELECT account.id account_id, account.telegram_id, account.discord_id, request_key, request_secret
            FROM twitter_auth
            JOIN account ON account.id = twitter_auth.account_id
            WHERE request_key = $1",
            request_key
        )
        .map(|row| TwitterRequest {
            account_id: row.account_id,
            telegram_id: row.telegram_id,
            discord_id: row.discord_id.and_then(|id| id.to_u64()),
            request_key: row.request_key,
            request_secret: row.request_secret,
        })
        .fetch_optional(conn)
        .await?;

        Ok(req)
    }

    /// Update a user's Twitter account with new credentials.
    ///
    /// Takes care of the following housekeeping items:
    /// * Deletes any previous accounts
    /// * Inserts the key and secret for the user
    /// * Deletes the pending request
    pub async fn set_account(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: User,
        creds: TwitterAccount,
    ) -> anyhow::Result<()> {
        let mut tx = conn.begin().await?;

        sqlx::query!(
            "DELETE FROM twitter_account
            WHERE account_id = lookup_account($1, $2)",
            user.telegram_id(),
            user.discord_id(),
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "DELETE FROM twitter_auth
            WHERE account_id = lookup_account($1, $2)",
            user.telegram_id(),
            user.discord_id(),
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "INSERT INTO twitter_account (account_id, consumer_key, consumer_secret) VALUES
                (lookup_account($1, $2), $3, $4)",
            user.telegram_id(),
            user.discord_id(),
            creds.consumer_key,
            creds.consumer_secret
        )
        .execute(&mut tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }

    pub async fn set_request<U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
        request_key: &str,
        request_secret: &str,
    ) -> anyhow::Result<()> {
        let user = user.into();

        let mut tx = conn.begin().await?;

        sqlx::query!(
            "DELETE FROM twitter_auth
            WHERE account_id = lookup_account($1, $2)",
            user.telegram_id(),
            user.discord_id(),
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "INSERT INTO twitter_auth (account_id, request_key, request_secret) VALUES
                (lookup_account($1, $2), $3, $4)",
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

    pub async fn remove_account<U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
    ) -> anyhow::Result<()> {
        let user = user.into();

        sqlx::query!(
            "DELETE FROM twitter_account
            WHERE account_id = lookup_account($1, $2)",
            user.telegram_id(),
            user.discord_id(),
        )
        .execute(conn)
        .await?;
        Ok(())
    }
}

pub struct FileCache;

impl FileCache {
    /// Look up a file's cached hash by its unique ID.
    pub async fn get(
        redis: &redis::aio::ConnectionManager,
        file_id: &str,
    ) -> anyhow::Result<Option<i64>> {
        use redis::AsyncCommands;

        let mut redis = redis.clone();

        let result = redis
            .get::<_, Option<i64>>(file_id)
            .await
            .map(|row| {
                let status = match row {
                    Some(_) => "hit",
                    None => "miss",
                };
                CACHE_REQUESTS
                    .get_metric_with_label_values(&[status])
                    .unwrap()
                    .inc();

                row
            })
            .context("unable to select hash from file_id_cache");

        result
    }

    pub async fn set(
        redis: &redis::aio::ConnectionManager,
        file_id: &str,
        hash: i64,
    ) -> anyhow::Result<()> {
        use redis::AsyncCommands;

        let mut redis = redis.clone();
        redis.set_ex(file_id, hash, 60 * 60 * 24 * 7).await?;

        Ok(())
    }
}

#[derive(sqlx::FromRow)]
pub struct Video {
    /// Database identifier of the video.
    pub id: i32,
    /// If the video has already been processed. If this is true, there must
    /// be an mp4_url.
    pub processed: bool,
    /// The original source of the video.
    pub source: String,
    /// The URL of the original video.
    pub url: String,
    /// The URL of the converted video.
    pub mp4_url: Option<String>,
    /// The URL of the converted video's thumbnail.
    pub thumb_url: Option<String>,
    /// The display URL for returning to the user when processing is complete.
    pub display_url: String,
    /// A unique display name representing the file's path and public ID.
    pub display_name: String,
    /// A job ID, if one exists, from Coconut.
    pub job_id: Option<i32>,
}

impl Video {
    /// Lookup a video by the display name.
    pub async fn lookup_display_name(
        conn: &sqlx::Pool<sqlx::Postgres>,
        display_name: &str,
    ) -> anyhow::Result<Option<Self>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url, thumb_url, display_url, display_name, job_id
            FROM videos
            WHERE display_name = $1",
            display_name
        )
        .fetch_optional(conn)
        .await?;

        Ok(video)
    }

    /// Lookup a video by the URL ID.
    pub async fn lookup_url_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        url_id: &str,
    ) -> anyhow::Result<Option<Self>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url, thumb_url, display_url, display_name, job_id
            FROM videos
            WHERE source = $1",
            url_id
        )
        .fetch_optional(conn)
        .await?;

        Ok(video)
    }

    /// Insert a new media item with a given URL ID and media URL.
    pub async fn insert_new_media(
        conn: &sqlx::Pool<sqlx::Postgres>,
        url_id: &str,
        media_url: &str,
        display_url: &str,
        display_name: &str,
    ) -> anyhow::Result<String> {
        let row = sqlx::query!(
            "INSERT INTO videos (source, url, display_url, display_name) VALUES
                ($1, $2, $3, $4)
            ON CONFLICT ON CONSTRAINT unique_source
                DO UPDATE SET source = EXCLUDED.source
            RETURNING display_name",
            url_id,
            media_url,
            display_url,
            display_name
        )
        .fetch_one(conn)
        .await?;

        Ok(row.display_name)
    }

    /// Set the Coconut job ID for the video.
    pub async fn set_job_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
        job_id: i32,
    ) -> anyhow::Result<()> {
        sqlx::query!("UPDATE videos SET job_id = $1 WHERE id = $2", job_id, id)
            .execute(conn)
            .await?;

        Ok(())
    }

    /// Update video by adding processed URLs.
    pub async fn set_processed_url(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
        mp4_url: &str,
        thumb_url: &str,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "UPDATE videos SET processed = true, mp4_url = $1, thumb_url = $2 WHERE id = $3",
            mp4_url,
            thumb_url,
            id
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    /// Add a new message associated with a video encoding job.
    pub async fn add_message_id<C: Into<Chat>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
        chat: C,
        message_id: i32,
    ) -> anyhow::Result<()> {
        let chat = chat.into();

        sqlx::query!(
            "INSERT INTO video_job_message (video_id, chat_id, message_id) VALUES
                ($1, lookup_chat($2, $3), $4)",
            id,
            chat.telegram_id(),
            chat.discord_id(),
            message_id
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    /// Get messages associated with a video job.
    pub async fn associated_messages(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
    ) -> anyhow::Result<Vec<(i64, i32)>> {
        // Dirty hack for making a best guess which chat ID is the latest.
        // Telegram's supergroups always seem to have higher IDs than the group
        // they were migrated from. This should have no impact on chats that
        // have never been migrated.
        // I don't think this actually matters because associated messages
        // should always be private messages.

        let ids = sqlx::query!(
            r#"SELECT
                message_id,
                (
                    SELECT chat_telegram.telegram_id
                    FROM chat_telegram
                    WHERE chat_id = video_job_message.chat_id
                    ORDER BY abs(chat_telegram.telegram_id) DESC
                    LIMIT 1
                ) as "chat_id!"
            FROM video_job_message
            WHERE video_id = $1"#,
            id
        )
        .map(|row| (row.chat_id, row.message_id))
        .fetch_all(conn)
        .await?;

        Ok(ids)
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
    pub async fn get(
        conn: &sqlx::Pool<sqlx::Postgres>,
        post_url: &str,
        thumb: bool,
    ) -> anyhow::Result<Option<Self>> {
        let post = sqlx::query!(
            "SELECT id, post_url, thumb, cdn_url, width, height
            FROM cached_post
            WHERE post_url = $1 AND thumb = $2",
            post_url,
            thumb
        )
        .fetch_optional(conn)
        .await?;

        let post = match post {
            Some(post) => post,
            None => return Ok(None),
        };

        Ok(Some(Self {
            id: post.id,
            post_url: post.post_url,
            thumb: post.thumb,
            cdn_url: post.cdn_url,
            dimensions: (post.width as u32, post.height as u32),
        }))
    }

    pub async fn save(
        conn: &sqlx::Pool<sqlx::Postgres>,
        post_url: &str,
        cdn_url: &str,
        thumb: bool,
        dimensions: (u32, u32),
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "INSERT INTO cached_post (post_url, thumb, cdn_url, width, height) VALUES
                ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            post_url,
            thumb,
            cdn_url,
            dimensions.0 as i64,
            dimensions.1 as i64
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct Permissions;

impl Permissions {
    pub async fn add_change(
        conn: &sqlx::Pool<sqlx::Postgres>,
        my_chat_member: &tgbotapi::ChatMemberUpdated,
    ) -> anyhow::Result<()> {
        let data = serde_json::to_value(&my_chat_member.new_chat_member).unwrap();

        sqlx::query!(
            "INSERT INTO permission (chat_id, updated_at, permissions) VALUES
                (lookup_chat_by_telegram_id($1), to_timestamp($2::int), $3)",
            my_chat_member.chat.id,
            my_chat_member.date,
            data
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct ChatAdmin;

impl ChatAdmin {
    /// Update a chat's known administrators from a ChatMemberUpdated event.
    ///
    /// Will discard updates that are older than the newest item in the
    /// database. Returns if the user is currently an admin.
    pub async fn update_chat<C: Into<Chat>, U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        status: &tgbotapi::ChatMemberStatus,
        user: U,
        chat: C,
        date: Option<i32>,
    ) -> anyhow::Result<bool> {
        use tgbotapi::ChatMemberStatus::*;

        let chat = chat.into();
        let user = user.into();

        let is_admin = matches!(status, Administrator | Creator);

        // It's possible updates get processed out of order. We only want to
        // update the table when the update occured more recently than the value
        // in the database.
        sqlx::query!(
            "INSERT INTO chat_administrator (account_id, chat_id, is_admin, updated_at)
                VALUES (lookup_account($1, $2), lookup_chat_by_telegram_id($3), $4, to_timestamp($5::bigint))",
            user.telegram_id(),
            user.discord_id(),
            chat.telegram_id(),
            is_admin,
            date.map(|time| time as i64).unwrap_or_else(|| chrono::Utc::now().timestamp()),
        )
        .execute(conn)
        .await?;

        Ok(is_admin)
    }

    /// Check if a user is currently an admin.
    pub async fn is_admin<U: Into<User>, C: Into<Chat>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
        chat: C,
    ) -> anyhow::Result<Option<bool>> {
        let user = user.into();
        let chat = chat.into();

        let is_admin = sqlx::query_scalar!(
            "SELECT is_admin
            FROM chat_administrator
            WHERE account_id = lookup_account($1, $2) AND chat_id = lookup_chat_by_telegram_id($3)
            ORDER BY updated_at DESC LIMIT 1",
            user.telegram_id(),
            user.discord_id(),
            chat.telegram_id(),
        )
        .fetch_optional(conn)
        .await?;

        Ok(is_admin)
    }

    /// Remove all known administrators from a group, except for the bot. We
    /// always get updates for our own user, so it's safe to keep.
    pub async fn flush<C: Into<Chat>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        bot_user_id: i64,
        chat: C,
    ) -> anyhow::Result<()> {
        let chat = chat.into();

        sqlx::query!(
            "DELETE FROM chat_administrator
            WHERE chat_id = lookup_chat_by_telegram_id($1) AND account_id <> lookup_account_by_telegram_id($2)",
            chat.telegram_id(),
            bot_user_id
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct Subscriptions;

pub struct Subscription {
    pub telegram_id: i64,
    pub hash: i64,
    pub message_id: Option<i32>,
    pub photo_id: Option<String>,
}

impl Subscriptions {
    pub async fn add_subscription<U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
        hash: i64,
        message_id: Option<i32>,
        photo_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let user = user.into();

        sqlx::query!(
            "INSERT INTO source_notification (account_id, hash, message_id, photo_id)
                VALUES (lookup_account($1, $2), $3, $4, $5) ON CONFLICT DO NOTHING",
            user.telegram_id(),
            user.discord_id(),
            hash,
            message_id,
            photo_id,
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn remove_subscription<U: Into<User>>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user: U,
        hash: i64,
    ) -> anyhow::Result<()> {
        let user = user.into();

        sqlx::query!(
            "DELETE FROM source_notification
            WHERE account_id = lookup_account($1, $2) AND hash <@ ($3, 0)",
            user.telegram_id(),
            user.discord_id(),
            hash
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn search_subscriptions(
        conn: &sqlx::Pool<sqlx::Postgres>,
        hash: i64,
    ) -> anyhow::Result<Vec<Subscription>> {
        let subscriptions = sqlx::query!(
            "SELECT account.telegram_id user_id, hash, message_id, photo_id
            FROM source_notification
            JOIN account ON account.id = source_notification.account_id
            WHERE hash <@ ($1, 3)",
            hash
        )
        .map(|row| Subscription {
            telegram_id: row.user_id.unwrap(),
            hash: row.hash.unwrap(),
            message_id: row.message_id,
            photo_id: row.photo_id,
        })
        .fetch_all(conn)
        .await?;

        Ok(subscriptions)
    }
}

pub struct FileURLCache;

impl FileURLCache {
    /// Look up a file's cached hash by a URL that is known to never change.
    pub async fn get(
        conn: &sqlx::Pool<sqlx::Postgres>,
        file_url: &str,
    ) -> anyhow::Result<Option<i64>> {
        let result = sqlx::query!("SELECT hash FROM file_url_cache WHERE url = $1", file_url)
            .fetch_optional(conn)
            .await
            .map(|row| row.map(|row| row.hash.unwrap()))
            .context("Unable to select hash from file_url_cache");

        result
    }

    pub async fn set(
        conn: &sqlx::Pool<sqlx::Postgres>,
        file_url: &str,
        hash: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "INSERT INTO file_url_cache (url, hash) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            file_url,
            hash
        )
        .execute(conn)
        .await?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
pub struct MediaGroup {
    pub id: i32,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub message: Message,
    pub sources: Option<Vec<fuzzysearch::File>>,
}

impl MediaGroup {
    /// Store a message as part of a media group, keeping track of when the
    /// value was inserted.
    pub async fn add_message(
        conn: &sqlx::Pool<sqlx::Postgres>,
        message: &Message,
    ) -> anyhow::Result<i32> {
        let id = sqlx::query_scalar!(
            "INSERT INTO media_group (media_group_id, inserted_at, message) VALUES ($1, $2, $3) RETURNING id",
            message.media_group_id.as_ref().unwrap(),
            chrono::Utc::now(),
            serde_json::to_value(message)?,
        )
        .fetch_one(conn)
        .await?;
        Ok(id)
    }

    /// Get the time that the last message was inserted to the media group.
    pub async fn last_message(
        conn: &sqlx::Pool<sqlx::Postgres>,
        media_group_id: &str,
    ) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
        let last_inserted_at = sqlx::query_scalar!(
            r#"SELECT max(inserted_at) FROM media_group WHERE media_group_id = $1"#,
            media_group_id
        )
        .fetch_optional(conn)
        .await?;
        Ok(last_inserted_at.flatten())
    }

    /// Get a specific stored message, if it exists.
    pub async fn get_message(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
    ) -> anyhow::Result<Option<MediaGroup>> {
        let message = sqlx::query!(
            "SELECT id, inserted_at, message, sources FROM media_group WHERE id = $1",
            id
        )
        .map(|row| MediaGroup {
            id: row.id,
            inserted_at: row.inserted_at,
            message: serde_json::from_value(row.message).unwrap(),
            sources: row
                .sources
                .map(|sources| serde_json::from_value(sources).unwrap()),
        })
        .fetch_optional(conn)
        .await?;

        Ok(message)
    }

    pub async fn get_messages(
        conn: &sqlx::Pool<sqlx::Postgres>,
        media_group_id: &str,
    ) -> anyhow::Result<Vec<MediaGroup>> {
        let messages = sqlx::query!(
            "SELECT id, inserted_at, message, sources FROM media_group WHERE media_group_id = $1",
            media_group_id
        )
        .map(|row| MediaGroup {
            id: row.id,
            inserted_at: row.inserted_at,
            message: serde_json::from_value(row.message).unwrap(),
            sources: row
                .sources
                .map(|sources| serde_json::from_value(sources).unwrap()),
        })
        .fetch_all(conn)
        .await?;
        Ok(messages)
    }

    /// Update the sources of a stored message, discarding any errors.
    pub async fn set_message_sources(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
        sources: Vec<fuzzysearch::File>,
    ) {
        let _ = sqlx::query!(
            "UPDATE media_group SET sources = $1 WHERE id = $2",
            serde_json::to_value(sources).unwrap(),
            id
        )
        .execute(conn)
        .await;
    }

    /// Consume (delete and return) all messages stored in the media group.
    pub async fn consume_messages(
        conn: &sqlx::Pool<sqlx::Postgres>,
        media_group_id: &str,
    ) -> anyhow::Result<Vec<MediaGroup>> {
        let messages = sqlx::query!(
            "DELETE FROM media_group WHERE media_group_id = $1 RETURNING id, inserted_at, message, sources",
            media_group_id
        )
        .map(|row| MediaGroup {
            id: row.id,
            inserted_at: row.inserted_at,
            message: serde_json::from_value(row.message).unwrap(),
            sources: row.sources.map(|sources| serde_json::from_value(sources).unwrap()),
        })
        .fetch_all(conn)
        .await?;
        Ok(messages)
    }

    /// Notify database that we are attempting to send a message, and return
    /// if we're allowed to send it.
    pub async fn sending_message(
        conn: &sqlx::Pool<sqlx::Postgres>,
        media_group_id: &str,
    ) -> anyhow::Result<bool> {
        let rows_affected = sqlx::query!(
            "INSERT INTO media_group_sent (media_group_id) VALUES ($1) ON CONFLICT DO NOTHING",
            media_group_id
        )
        .execute(conn)
        .await?
        .rows_affected();

        Ok(rows_affected == 1)
    }

    /// Delete all information related to a media group.
    pub async fn purge_media_group(
        conn: &sqlx::Pool<sqlx::Postgres>,
        media_group_id: &str,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM media_group WHERE media_group_id = $1",
            media_group_id
        )
        .execute(conn)
        .await?;
        sqlx::query!(
            "DELETE FROM media_group_sent WHERE media_group_id = $1",
            media_group_id
        )
        .execute(conn)
        .await?;
        Ok(())
    }
}
