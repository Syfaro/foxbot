use anyhow::Context;

lazy_static::lazy_static! {
    static ref CACHE_REQUESTS: prometheus::CounterVec = prometheus::register_counter_vec!("foxbot_cache_requests_total", "Number of file cache hits and misses", &["result"]).unwrap();
}

/// Each available site, for configuration usage.
#[derive(Clone, Debug, PartialEq)]
pub enum Sites {
    FurAffinity,
    E621,
    Twitter,
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
            "FurAffinity" => Ok(Sites::FurAffinity),
            "e621" => Ok(Sites::E621),
            "Twitter" => Ok(Sites::Twitter),
            _ => Err(ParseSitesError),
        }
    }
}

impl Sites {
    /// Get the user-understandable name of the site.
    pub fn as_str(&self) -> &'static str {
        match *self {
            Sites::FurAffinity => "FurAffinity",
            Sites::E621 => "e621",
            Sites::Twitter => "Twitter",
        }
    }

    /// The bot's default site ordering.
    pub fn default_order() -> Vec<Sites> {
        vec![Sites::FurAffinity, Sites::E621, Sites::Twitter]
    }
}

pub struct UserConfig;

pub enum UserConfigKey {
    SourceName,
    SiteSortOrder,
}

impl UserConfigKey {
    fn as_str(&self) -> &str {
        match self {
            UserConfigKey::SourceName => "source-name",
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
            "SELECT value FROM user_config WHERE user_id = $1 AND name = $2",
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
        key: &str,
        user_id: i64,
        data: T,
    ) -> anyhow::Result<()> {
        let value = serde_json::to_value(&data)?;

        sqlx::query!(
            "
            INSERT INTO user_config (user_id, name, value)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id, name)
            DO
                UPDATE SET value = EXCLUDED.value
        ",
            user_id,
            key,
            value
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
    HasDeletePermission,
}

impl GroupConfigKey {
    fn as_str(&self) -> &str {
        match self {
            GroupConfigKey::GroupAdd => "group_add",
            GroupConfigKey::GroupNoPreviews => "group_no_previews",
            GroupConfigKey::HasDeletePermission => "has_delete_permission",
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
            "SELECT value FROM group_config WHERE chat_id = $1 AND name = $2",
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
            "
            INSERT INTO group_config (chat_id, name, value)
            VALUES ($1, $2, $3)
            ON CONFLICT (chat_id, name)
            DO
                UPDATE SET value = EXCLUDED.value
        ",
            chat_id,
            key.as_str(),
            value
        )
        .execute(conn)
        .await?;

        Ok(())
    }

    pub async fn delete(
        conn: &sqlx::Pool<sqlx::Postgres>,
        key: GroupConfigKey,
        chat_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM group_config WHERE chat_id = $1 AND name = $2",
            chat_id,
            key.as_str()
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
            "SELECT consumer_key, consumer_secret FROM twitter_account WHERE user_id = $1",
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
            "SELECT user_id, request_key, request_secret FROM twitter_auth WHERE request_key = $1",
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

        sqlx::query!("DELETE FROM twitter_account WHERE user_id = $1", user_id)
            .execute(&mut tx)
            .await?;
        sqlx::query!("DELETE FROM twitter_auth WHERE user_id = $1", user_id)
            .execute(&mut tx)
            .await?;
        sqlx::query!("INSERT INTO twitter_account (user_id, consumer_key, consumer_secret) VALUES ($1, $2, $3)", user_id, creds.consumer_key, creds.consumer_secret).execute(&mut tx).await?;

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

        sqlx::query!("DELETE FROM twitter_auth WHERE user_id = $1", &user_id)
            .execute(&mut tx)
            .await?;
        sqlx::query!(
            "INSERT INTO twitter_auth (user_id, request_key, request_secret) VALUES ($1, $2, $3)",
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
        sqlx::query!("DELETE FROM twitter_account WHERE user_id = $1", user_id)
            .execute(conn)
            .await?;
        Ok(())
    }
}

pub struct FileCache;

impl FileCache {
    /// Look up a file's cached hash by its unique ID.
    pub async fn get(
        conn: &sqlx::Pool<sqlx::Postgres>,
        file_id: &str,
    ) -> anyhow::Result<Option<i64>> {
        let result = sqlx::query!("SELECT hash FROM file_id_cache WHERE file_id = $1", file_id)
            .fetch_optional(conn)
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

                row.map(|row| row.hash)
            })
            .context("unable to select hash from file_id_cache");

        result
    }

    pub async fn set(
        conn: &sqlx::Pool<sqlx::Postgres>,
        file_id: &str,
        hash: i64,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "INSERT INTO file_id_cache (file_id, hash) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            file_id,
            hash
        )
        .execute(conn)
        .await?;

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
    ) -> anyhow::Result<Option<Video>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url, thumb_url, display_url, display_name, job_id FROM videos WHERE display_name = $1",
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
    ) -> anyhow::Result<Option<Video>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url, thumb_url, display_url, display_name, job_id FROM videos WHERE source = $1",
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
            "INSERT INTO videos (source, url, display_url, display_name) VALUES ($1, $2, $3, $4) ON CONFLICT ON CONSTRAINT unique_source DO UPDATE SET source = EXCLUDED.source RETURNING display_name",
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
            "INSERT INTO video_job_message (video_id, chat_id, message_id) VALUES ($1, $2, $3)",
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
        let ids = sqlx::query!(
            "SELECT chat_id, message_id FROM video_job_message WHERE video_id = $1",
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
    ) -> anyhow::Result<Option<CachedPost>> {
        let post = sqlx::query!("SELECT id, post_url, thumb, cdn_url, width, height FROM cached_post WHERE post_url = $1 AND thumb = $2", post_url, thumb).fetch_optional(conn).await?;

        let post = match post {
            Some(post) => post,
            None => return Ok(None),
        };

        Ok(Some(CachedPost {
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
        let row = sqlx::query!("INSERT INTO cached_post (post_url, thumb, cdn_url, width, height) VALUES ($1, $2, $3, $4, $5) RETURNING id", post_url, thumb, cdn_url, dimensions.0 as i64, dimensions.1 as i64).fetch_one(conn).await?;

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
            "INSERT INTO permission (chat_id, updated_at, permissions) VALUES ($1, to_timestamp($2::int), $3)",
            my_chat_member.chat.id,
            my_chat_member.date,
            data
        )
        .execute(conn).await?;

        Ok(())
    }
}

pub struct ChatAdmin;

impl ChatAdmin {
    /// Update a chat's known administrators from a ChatMemberUpdated event.
    ///
    /// Will discard updates that are older than the newest item in the
    /// database. Returns if the value was updated, and if it was, if the user
    /// is currently an admin.
    pub async fn update_chat(
        conn: &sqlx::Pool<sqlx::Postgres>,
        data: &tgbotapi::ChatMemberUpdated,
    ) -> anyhow::Result<Option<bool>> {
        use tgbotapi::ChatMemberStatus::*;

        let is_admin = match data.new_chat_member.status {
            Administrator | Creator => true,
            _ => false,
        };

        // It's possible updates get processed out of order. We only want to
        // update the table when the update occured more recently than the value
        // in the database.
        let is_admin = sqlx::query_scalar!(
            "WITH data (user_id, chat_id, is_admin, last_update) AS (
                VALUES ($1::bigint, $2::bigint, $3::boolean, to_timestamp($4::int))
            )
            INSERT INTO chat_administrator
            SELECT data.user_id, data.chat_id, data.is_admin, data.last_update FROM data
            LEFT JOIN chat_administrator ON data.user_id = chat_administrator.user_id AND data.chat_id = chat_administrator.chat_id
            WHERE chat_administrator.last_update IS NULL OR data.last_update > chat_administrator.last_update
            ON CONFLICT (user_id, chat_id) DO UPDATE SET is_admin = EXCLUDED.is_admin, last_update = EXCLUDED.last_update
            RETURNING is_admin",
            data.new_chat_member.user.id,
            data.chat.id,
            is_admin,
            data.date,
        )
        .fetch_optional(conn)
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
            "SELECT is_admin FROM chat_administrator WHERE user_id = $1 AND chat_id = $2",
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
            "DELETE FROM chat_administrator WHERE chat_id = $1 AND user_id <> $2",
            chat_id,
            bot_user_id
        )
        .execute(conn)
        .await?;

        Ok(())
    }
}
