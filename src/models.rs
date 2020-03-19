use quaint::pooled::PooledConnection;
use quaint::prelude::*;

static USER_CONFIG: &str = "user_config";
static TWITTER_ACCOUNT: &str = "twitter_account";
static TWITTER_AUTH: &str = "twitter_auth";
static FILE_ID_CACHE: &str = "file_id_cache";
static GROUP_CONFIG: &str = "group_config";

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

struct Config;

impl Config {
    /// Execute a query and parse the value field from JSON into `T`.
    async fn get<T: serde::de::DeserializeOwned>(
        conn: &PooledConnection,
        select: quaint::ast::Select<'_>,
    ) -> failure::Fallible<Option<T>> {
        let rows = conn.select(select).await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows.into_single()?;
        let value = match row["value"].as_str() {
            Some(val) => val,
            _ => return Ok(None),
        };

        let data: T = serde_json::from_str(&value)?;

        Ok(Some(data))
    }

    async fn delete(
        conn: &PooledConnection,
        delete: quaint::ast::Delete<'_>,
    ) -> quaint::Result<()> {
        conn.delete(delete).await
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
        conn: &PooledConnection,
        key: UserConfigKey,
        user_id: i32,
    ) -> failure::Fallible<Option<T>> {
        let select = Select::from_table(USER_CONFIG)
            .so_that("user_id".equals(user_id).and("name".equals(key.as_str())));

        Config::get(&conn, select).await
    }

    /// Set a configuration value for the user_config table.
    pub async fn set<T: serde::Serialize>(
        conn: &PooledConnection,
        key: &str,
        user_id: i32,
        update: bool,
        data: T,
    ) -> failure::Fallible<()> {
        let value = serde_json::to_string(&data)?;

        if update {
            let update = Update::table(USER_CONFIG)
                .set("value", value)
                .so_that("user_id".equals(user_id).and("name".equals(key)));
            conn.update(update).await?;
        } else {
            let insert = Insert::single_into(USER_CONFIG)
                .value("user_id", user_id)
                .value("name", key)
                .value("value", value)
                .build();
            conn.insert(insert).await?;
        }

        Ok(())
    }
}

pub struct GroupConfig;

pub enum GroupConfigKey {
    GroupAdd,
}

impl GroupConfigKey {
    fn as_str(&self) -> &str {
        match self {
            GroupConfigKey::GroupAdd => "group_add",
        }
    }
}

impl GroupConfig {
    pub async fn get<T: serde::de::DeserializeOwned>(
        conn: &PooledConnection,
        chat_id: i64,
        name: GroupConfigKey,
    ) -> failure::Fallible<Option<T>> {
        let select = Select::from_table(GROUP_CONFIG)
            .so_that("chat_id".equals(chat_id).and("name".equals(name.as_str())));
        Config::get(&conn, select).await
    }

    pub async fn set<T: serde::Serialize>(
        conn: &PooledConnection,
        key: GroupConfigKey,
        chat_id: i64,
        update: bool,
        data: T,
    ) -> failure::Fallible<()> {
        let value = serde_json::to_string(&data)?;

        if update {
            let update = Update::table(GROUP_CONFIG)
                .set("value", value)
                .so_that("chat_id".equals(chat_id).and("name".equals(key.as_str())));
            conn.update(update).await?;
        } else {
            let insert = Insert::single_into(GROUP_CONFIG)
                .value("chat_id", chat_id)
                .value("name", key.as_str())
                .value("value", value)
                .build();
            conn.insert(insert).await?;
        }

        Ok(())
    }

    pub async fn delete(
        conn: &PooledConnection,
        key: GroupConfigKey,
        chat_id: i64,
    ) -> quaint::Result<()> {
        let delete = Delete::from_table(GROUP_CONFIG)
            .so_that("chat_id".equals(chat_id).and("name".equals(key.as_str())));
        Config::delete(&conn, delete).await
    }
}

/// A Twitter account, as stored within the database.
pub struct TwitterAccount {
    pub consumer_key: String,
    pub consumer_secret: String,
}

pub struct TwitterRequest {
    pub request_key: String,
    pub request_secret: String,
}

pub struct Twitter;

impl Twitter {
    /// Look up a user's Twitter credentials.
    pub async fn get_account(
        conn: &PooledConnection,
        user_id: i32,
    ) -> quaint::Result<Option<TwitterAccount>> {
        let select = Select::from_table(TWITTER_ACCOUNT)
            .column("consumer_key")
            .column("consumer_secret")
            .so_that("user_id".equals(user_id));
        let rows = conn.select(select).await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows.into_single()?;

        Ok(Some(TwitterAccount {
            consumer_key: row["consumer_key"].to_string().unwrap(),
            consumer_secret: row["consumer_secret"].to_string().unwrap(),
        }))
    }

    /// Look up a pending request to sign into a Twitter account.
    pub async fn get_request(
        conn: &PooledConnection,
        user_id: i32,
    ) -> quaint::Result<Option<TwitterRequest>> {
        let select = Select::from_table(TWITTER_AUTH)
            .column("request_key")
            .column("request_secret")
            .so_that("user_id".equals(user_id));
        let rows = conn.select(select).await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows.into_single()?;

        Ok(Some(TwitterRequest {
            request_key: row["request_key"].to_string().unwrap(),
            request_secret: row["request_secret"].to_string().unwrap(),
        }))
    }

    /// Update a user's Twitter account with new credentials.
    ///
    /// Takes care of the following housekeeping items:
    /// * Deletes any previous accounts
    /// * Inserts the key and secret for the user
    /// * Deletes the pending request
    pub async fn set_account(
        conn: &PooledConnection,
        user_id: i32,
        creds: TwitterAccount,
    ) -> quaint::Result<()> {
        let delete = Delete::from_table(TWITTER_ACCOUNT).so_that("user_id".equals(user_id));
        conn.delete(delete).await?;

        let insert = Insert::single_into(TWITTER_ACCOUNT)
            .value("user_id", user_id)
            .value("consumer_key", creds.consumer_key)
            .value("consumer_secret", creds.consumer_secret)
            .build();
        conn.insert(insert).await?;

        let delete = Delete::from_table(TWITTER_AUTH).so_that("user_id".equals(user_id));
        conn.delete(delete).await?;

        Ok(())
    }

    pub async fn set_request(
        conn: &PooledConnection,
        user_id: i32,
        creds: TwitterRequest,
    ) -> quaint::Result<()> {
        let delete = Delete::from_table(TWITTER_AUTH).so_that("user_id".equals(user_id));
        conn.delete(delete).await?;

        let insert = Insert::single_into(TWITTER_AUTH)
            .value("user_id", user_id)
            .value("request_key", creds.request_key)
            .value("request_secret", creds.request_secret)
            .build();
        conn.insert(insert).await?;

        Ok(())
    }
}

pub struct FileCache;

impl FileCache {
    /// Look up a file's cached hash by its unique ID.
    pub async fn get(conn: &PooledConnection, file_id: &str) -> quaint::Result<Option<i64>> {
        let select = Select::from_table(FILE_ID_CACHE)
            .column("hash")
            .so_that("file_id".equals(file_id));
        let rows = conn.select(select).await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let row = rows.into_single()?;
        Ok(Some(row["hash"].as_i64().unwrap()))
    }

    pub async fn set(conn: &PooledConnection, file_id: &str, hash: i64) -> quaint::Result<()> {
        let insert = Insert::single_into(FILE_ID_CACHE)
            .value("file_id", file_id)
            .value("hash", hash)
            .build();
        conn.insert(insert).await.map(|_| ())
    }
}
