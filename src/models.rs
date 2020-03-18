use quaint::prelude::*;

static USER_CONFIG: &str = "user_config";

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

impl Sites {
    /// Get the user-understandable name of the site.
    pub fn as_str(&self) -> &'static str {
        match *self {
            Sites::FurAffinity => "FurAffinity",
            Sites::E621 => "e621",
            Sites::Twitter => "Twitter",
        }
    }

    /// Parse a user-understandable name of a site.
    ///
    /// Will panic if an invalid site name is provided.
    pub fn from_str(s: &str) -> Sites {
        match s {
            "FurAffinity" => Sites::FurAffinity,
            "e621" => Sites::E621,
            "Twitter" => Sites::Twitter,
            _ => panic!("Invalid value"),
        }
    }

    /// The bot's default site ordering.
    pub fn default_order() -> Vec<Sites> {
        vec![Sites::FurAffinity, Sites::E621, Sites::Twitter]
    }
}

/// Get a configuration value from the user_config table.
///
/// If the value does not exist for a given user, returns None.
pub async fn get_user_config<T: serde::de::DeserializeOwned>(
    conn: &quaint::pooled::PooledConnection,
    key: &str,
    user_id: i32,
) -> failure::Fallible<Option<T>> {
    let select =
        Select::from_table(USER_CONFIG).so_that("user_id".equals(user_id).and("name".equals(key)));
    let rows = conn.select(select).await?;

    if rows.is_empty() {
        return Ok(None);
    }

    let row = rows.into_single()?;
    let value = match row["value"].as_str() {
        Some(val) => val,
        None => return Ok(None),
    };

    let data: T = serde_json::from_str(&value)?;

    Ok(Some(data))
}

/// Set a configuration value for the user_config table.
pub async fn set_user_config<T: serde::Serialize>(
    conn: &quaint::pooled::PooledConnection,
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
