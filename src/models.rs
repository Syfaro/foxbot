#![allow(clippy::suspicious_else_formatting)] // SQLx queries seem to generate these warnings...
#![allow(clippy::toplevel_ref_arg)] // SQLx also generates these, should be fixed in next release

use anyhow::Context;

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
        user_id: i32,
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
        user_id: i32,
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
    IsAdmin,
    GroupNoPreviews,
}

impl GroupConfigKey {
    fn as_str(&self) -> &str {
        match self {
            GroupConfigKey::GroupAdd => "group_add",
            GroupConfigKey::IsAdmin => "is_admin",
            GroupConfigKey::GroupNoPreviews => "group_no_previews",
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
    pub request_key: String,
    pub request_secret: String,
}

pub struct Twitter;

impl Twitter {
    /// Look up a user's Twitter credentials.
    pub async fn get_account(
        conn: &sqlx::Pool<sqlx::Postgres>,
        user_id: i32,
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
        user_id: i32,
    ) -> anyhow::Result<Option<TwitterRequest>> {
        let req = sqlx::query_as!(
            TwitterRequest,
            "SELECT request_key, request_secret FROM twitter_auth WHERE user_id = $1",
            user_id
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
        user_id: i32,
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
        user_id: i32,
        creds: TwitterRequest,
    ) -> anyhow::Result<()> {
        let mut tx = conn.begin().await?;

        sqlx::query!("DELETE FROM twitter_auth WHERE user_id = $1", &user_id)
            .execute(&mut tx)
            .await?;
        sqlx::query!(
            "INSERT INTO twitter_auth (user_id, request_key, request_secret) VALUES ($1, $2, $3)",
            user_id,
            creds.request_key,
            creds.request_secret
        )
        .execute(&mut tx)
        .await?;

        tx.commit().await?;

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
        sqlx::query!("SELECT hash FROM file_id_cache WHERE file_id = $1", file_id)
            .fetch_optional(conn)
            .await
            .map(|row| row.map(|row| row.hash))
            .context("unable to select hash from file_id_cache")
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
}

impl Video {
    /// Lookup a Video by ID.
    pub async fn lookup_id(
        conn: &sqlx::Pool<sqlx::Postgres>,
        id: i32,
    ) -> anyhow::Result<Option<Video>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url FROM videos WHERE id = $1",
            id
        )
        .fetch_optional(conn)
        .await?;

        Ok(video)
    }

    /// Lookup a Video by URL.
    pub async fn lookup_url(
        conn: &sqlx::Pool<sqlx::Postgres>,
        url: &str,
    ) -> anyhow::Result<Option<Video>> {
        let video = sqlx::query_as!(
            Video,
            "SELECT id, processed, source, url, mp4_url FROM videos WHERE url = $1",
            url
        )
        .fetch_optional(conn)
        .await?;

        Ok(video)
    }

    /// Insert a new URL into the database and return the ID.
    pub async fn insert_url(
        conn: &sqlx::Pool<sqlx::Postgres>,
        url: &str,
        source: &str,
    ) -> anyhow::Result<i32> {
        let row = sqlx::query!(
            "INSERT INTO videos (url, source) VALUES ($1, $2) RETURNING id",
            url,
            source
        )
        .fetch_one(conn)
        .await?;

        Ok(row.id)
    }

    /// Update a video's mp4_url when it has been processed.
    pub async fn set_processed_url(
        conn: &sqlx::Pool<sqlx::Postgres>,
        url: &str,
        mp4_url: &str,
    ) -> anyhow::Result<()> {
        sqlx::query!(
            "UPDATE videos SET processed = true, mp4_url = $1 WHERE url = $2",
            mp4_url,
            url
        )
        .execute(conn)
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
