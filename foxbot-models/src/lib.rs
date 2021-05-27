use anyhow::Context;

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
    pub async fn get<T: serde::de::DeserializeOwned>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: UserConfigKey,
        user_id: i64,
    ) -> anyhow::Result<Option<T>> {
        sqlx::query!(
            "SELECT value
            FROM user_config
            WHERE user_config.account_id = lookup_account_by_telegram_id($1) AND name = $2
            ORDER BY updated_at DESC LIMIT 1",
            user_id,
            key.as_str()
        )
        .fetch_optional(conn)
        .await
        .map(|row| row.map(|row| serde_json::from_value(row.value).unwrap()))
        .context("unable to perform user_config lookup")
    }

    /// Set a configuration value for the user_config table.
    pub async fn set<T: serde::Serialize>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: UserConfigKey,
        user_id: i64,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(&data)?;

        sqlx::query!(
            "INSERT INTO user_config (account_id, name, value)
            VALUES (lookup_account_by_telegram_id($1), $2, $3)",
            user_id,
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
        user_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM user_config
            WHERE account_id = lookup_account_by_telegram_id($1) AND name = $2",
            user_id,
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
    pub async fn get<T: serde::de::DeserializeOwned>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        chat_id: i64,
        name: GroupConfigKey,
    ) -> anyhow::Result<Option<T>> {
        sqlx::query!(
            "SELECT value
            FROM group_config
            WHERE group_config.chat_id = lookup_chat_by_telegram_id($1) AND name = $2
            ORDER BY updated_at DESC LIMIT 1",
            chat_id,
            name.as_str()
        )
        .fetch_optional(conn)
        .await
        .map(|row| row.map(|row| serde_json::from_value(row.value).unwrap()))
        .context("unable to perform group_config lookup")
    }

    pub async fn set<T: serde::Serialize>(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: GroupConfigKey,
        chat_id: i64,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(data)?;

        sqlx::query!(
            "INSERT INTO group_config (chat_id, name, value) VALUES
                (lookup_chat_by_telegram_id($1), $2, $3)",
            chat_id,
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
    pub user_id: i64,
    pub request_key: String,
    pub request_secret: String,
}

pub struct Twitter;

impl Twitter {
    /// Look up a user's Twitter credentials.
    pub async fn get_account(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
    ) -> anyhow::Result<Option<TwitterAccount>> {
        let account = sqlx::query_as!(
            TwitterAccount,
            "SELECT consumer_key, consumer_secret
            FROM twitter_account
            WHERE twitter_account.account_id = lookup_account_by_telegram_id($1)",
            user_id
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
        let req = sqlx::query_as!(
            TwitterRequest,
            "SELECT account.telegram_id user_id, request_key, request_secret
            FROM twitter_auth
            JOIN account ON account.id = twitter_auth.account_id
            WHERE request_key = $1",
            request_key
        )
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
        user_id: i64,
        creds: TwitterAccount,
    ) -> anyhow::Result<()> {
        let mut tx = conn.begin().await?;

        sqlx::query!(
            "DELETE FROM twitter_account
            WHERE account_id = lookup_account_by_telegram_id($1)",
            user_id
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "DELETE FROM twitter_auth
            WHERE account_id = lookup_account_by_telegram_id($1)",
            user_id
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "INSERT INTO twitter_account (account_id, consumer_key, consumer_secret) VALUES
                (lookup_account_by_telegram_id($1), $2, $3)",
            user_id,
            creds.consumer_key,
            creds.consumer_secret
        )
        .execute(&mut tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }

    pub async fn set_request(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
        request_key: &str,
        request_secret: &str,
    ) -> anyhow::Result<()> {
        let mut tx = conn.begin().await?;

        sqlx::query!(
            "DELETE FROM twitter_auth
            WHERE account_id = lookup_account_by_telegram_id($1)",
            &user_id
        )
        .execute(&mut tx)
        .await?;
        sqlx::query!(
            "INSERT INTO twitter_auth (account_id, request_key, request_secret) VALUES
                (lookup_account_by_telegram_id($1), $2, $3)",
            user_id,
            request_key,
            request_secret
        )
        .execute(&mut tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }

    pub async fn remove_account(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM twitter_account
            WHERE account_id = lookup_account_by_telegram_id($1)",
            user_id
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
    pub async fn add_message_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
        chat_id: i64,
        message_id: i32,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "INSERT INTO video_job_message (video_id, chat_id, message_id) VALUES
                ($1, lookup_chat_by_telegram_id($2), $3)",
            id,
            chat_id,
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
    ) -> anyhow::Result<i32> {
        let row = sqlx::query!(
            "INSERT INTO cached_post (post_url, thumb, cdn_url, width, height) VALUES
                ($1, $2, $3, $4, $5) RETURNING id",
            post_url,
            thumb,
            cdn_url,
            dimensions.0 as i64,
            dimensions.1 as i64
        )
        .fetch_one(conn)
        .await?;

        Ok(row.id)
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
    pub async fn update_chat(
        conn: &sqlx::Pool<sqlx::Postgres>,
        status: &tgbotapi::ChatMemberStatus,
        user_id: i64,
        chat_id: i64,
        date: Option<i32>,
    ) -> anyhow::Result<bool> {
        use tgbotapi::ChatMemberStatus::*;

        let is_admin = matches!(status, Administrator | Creator);

        // It's possible updates get processed out of order. We only want to
        // update the table when the update occured more recently than the value
        // in the database.
        sqlx::query!(
            "INSERT INTO chat_administrator (account_id, chat_id, is_admin, updated_at)
                VALUES (lookup_account_by_telegram_id($1), lookup_chat_by_telegram_id($2), $3, to_timestamp($4::bigint))",
            user_id,
            chat_id,
            is_admin,
            date.map(|time| time as i64).unwrap_or_else(|| chrono::Utc::now().timestamp()),
        )
        .execute(conn)
        .await?;

        Ok(is_admin)
    }

    /// Check if a user is currently an admin.
    pub async fn is_admin(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
        chat_id: i64,
    ) -> anyhow::Result<Option<bool>> {
        let is_admin = sqlx::query_scalar!(
            "SELECT is_admin
            FROM chat_administrator
            WHERE account_id = lookup_account_by_telegram_id($1) AND chat_id = lookup_chat_by_telegram_id($2)
            ORDER BY updated_at DESC LIMIT 1",
            user_id,
            chat_id
        )
        .fetch_optional(conn)
        .await?;

        Ok(is_admin)
    }

    /// Remove all known administrators from a group, except for the bot. We
    /// always get updates for our own user, so it's safe to keep.
    pub async fn flush(
        conn: &sqlx::Pool<sqlx::Postgres>,
        bot_user_id: i64,
        chat_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM chat_administrator
            WHERE chat_id = lookup_chat_by_telegram_id($1) AND account_id <> lookup_account_by_telegram_id($2)",
            chat_id,
            bot_user_id
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}

pub struct Subscriptions;

pub struct Subscription {
    pub user_id: i64,
    pub hash: i64,
    pub message_id: Option<i32>,
    pub photo_id: Option<String>,
}

impl Subscriptions {
    pub async fn add_subscription(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
        hash: i64,
        message_id: Option<i32>,
        photo_id: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "INSERT INTO source_notification (account_id, hash, message_id, photo_id)
                VALUES (lookup_account_by_telegram_id($1), $2, $3, $4) ON CONFLICT DO NOTHING",
            user_id,
            hash,
            message_id,
            photo_id,
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn remove_subscription(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i64,
        hash: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM source_notification
            WHERE account_id = lookup_account_by_telegram_id($1) AND hash <@ ($2, 0)",
            user_id,
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
            user_id: row.user_id.unwrap(),
            hash: row.hash.unwrap(),
            message_id: row.message_id,
            photo_id: row.photo_id,
        })
        .fetch_all(conn)
        .await?;

        Ok(subscriptions)
    }
}
