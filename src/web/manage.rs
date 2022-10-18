use std::{collections::HashMap, pin::Pin};

use actix_web::{get, http::header, post, web, HttpRequest, HttpResponse};
use askama::Template;
use chrono::TimeZone;
use futures::Future;
use hmac::{Hmac, Mac};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::models;

use super::Config;

pub fn scope() -> actix_web::Scope {
    web::scope("/manage").service(actix_web::services![
        manage_login,
        manage_list,
        manage_telegram_chat,
        manage_telegram_chat_post
    ])
}

async fn get_chat(
    bot: &tgbotapi::Telegram,
    redis: &mut redis::aio::ConnectionManager,
    chat_id: i64,
) -> Result<tgbotapi::Chat, actix_web::Error> {
    let cache_key = format!("chat:{}", chat_id);

    if let Some(data) = redis
        .get::<_, Option<Vec<u8>>>(&cache_key)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?
    {
        if let Ok(chat) = serde_json::from_slice(&data) {
            return Ok(chat);
        } else {
            tracing::warn!("serialized chat data could not be decoded");
        }
    }

    let get_chat = tgbotapi::requests::GetChat {
        chat_id: chat_id.into(),
    };

    let chat = bot
        .make_request(&get_chat)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let data = serde_json::to_vec(&chat).map_err(actix_web::error::ErrorInternalServerError)?;
    redis
        .set_ex(cache_key, data, 60 * 60 * 24)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(chat)
}

struct TelegramUser {
    id: i64,
    first_name: String,
}

impl actix_web::FromRequest for TelegramUser {
    type Error = actix_web::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        payload: &mut actix_web::dev::Payload,
    ) -> Self::Future {
        let secret = req
            .app_data::<web::Data<Config>>()
            .map(|config| config.manage_jwt_secret.to_owned());

        let session = actix_session::Session::from_request(req, payload);

        Box::pin(async move {
            let secret = secret.ok_or_else(|| {
                actix_web::error::ErrorInternalServerError("server misconfiguration")
            })?;

            let session = session
                .await
                .map_err(actix_web::error::ErrorInternalServerError)?;

            let jwt: String = session
                .get("jwt")
                .map_err(actix_web::error::ErrorInternalServerError)?
                .ok_or_else(|| actix_web::error::ErrorUnauthorized("missing token"))?;

            let key = jsonwebtoken::DecodingKey::from_secret(&secret);
            let validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS512);

            let data = jsonwebtoken::decode::<Claims>(&jwt, &key, &validation)
                .map_err(actix_web::error::ErrorBadRequest)?;

            let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS512);
            let expiration = (chrono::Utc::now() + chrono::Duration::minutes(30)).timestamp();

            let claims = Claims {
                sub: data.claims.sub,
                exp: expiration,
                first_name: data.claims.first_name.clone(),
            };

            let key = jsonwebtoken::EncodingKey::from_secret(&secret);

            let token = jsonwebtoken::encode(&header, &claims, &key)
                .map_err(actix_web::error::ErrorInternalServerError)?;

            session.insert("jwt", token)?;

            Ok(Self {
                id: data.claims.sub,
                first_name: data.claims.first_name,
            })
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Claims {
    sub: i64,
    exp: i64,

    first_name: String,
}

#[get("/login", name = "manage_login")]
async fn manage_login(
    request: HttpRequest,
    config: web::Data<Config>,
    query: web::Query<HashMap<String, String>>,
    session: actix_session::Session,
) -> actix_web::Result<HttpResponse, actix_web::Error> {
    let mut query = query.into_inner();

    let hash = query
        .remove("hash")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing required data"))?;
    let hash = hex::decode(hash).map_err(actix_web::error::ErrorBadRequest)?;

    let id: i64 = query
        .get("id")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing id"))?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let first_name = query
        .get("first_name")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing first_name"))?
        .to_owned();

    let auth_date: i64 = query
        .get("auth_date")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing auth_date"))?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let auth_date = chrono::Utc.timestamp(auth_date, 0);
    if auth_date + chrono::Duration::minutes(15) < chrono::Utc::now() {
        return Err(actix_web::error::ErrorBadRequest("auth_date too old"));
    }

    let mut data: Vec<_> = query.into_iter().collect();
    data.sort_by(|a, b| a.0.cmp(&b.0));

    let data = data
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join("\n");

    let token = sha2::Sha256::digest(&config.telegram_api_key);

    let mut mac = Hmac::<Sha256>::new_from_slice(&token)
        .expect("hmac could not be constructed with provided token");
    mac.update(data.as_bytes());

    mac.verify_slice(&hash)
        .map_err(actix_web::error::ErrorBadRequest)?;

    let expiration = (chrono::Utc::now() + chrono::Duration::minutes(30)).timestamp();

    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS512);

    let claims = Claims {
        sub: id,
        exp: expiration,
        first_name,
    };

    let key = jsonwebtoken::EncodingKey::from_secret(&config.manage_jwt_secret);

    let token = jsonwebtoken::encode(&header, &claims, &key)
        .map_err(actix_web::error::ErrorInternalServerError)?;

    session
        .insert("jwt", token)
        .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Found()
        .insert_header((
            actix_web::http::header::LOCATION,
            request.url_for_static("manage_list").unwrap().as_str(),
        ))
        .finish())
}

fn chat_display_name(chat: &tgbotapi::Chat) -> &str {
    if let Some(title) = &chat.title {
        title
    } else if let Some(username) = &chat.username {
        username
    } else if let Some(first_name) = &chat.first_name {
        first_name
    } else {
        "Unknown"
    }
}

#[derive(Template)]
#[template(path = "manage/list.html")]
struct ListGroupsTemplate {
    user_name: String,
    chats: Vec<(i64, String)>,
}

#[get("/chats", name = "manage_list")]
async fn manage_list(
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    user: TelegramUser,
) -> actix_web::Result<HttpResponse, actix_web::Error> {
    let chat_ids = models::Manage::get_user_groups(&pool, models::User::Telegram(user.id))
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let bot = tgbotapi::Telegram::new(config.telegram_api_key.clone());

    let mut chats = Vec::with_capacity(chat_ids.len());

    let mut redis = (*redis.into_inner()).clone();

    for chat_id in chat_ids {
        let chat = get_chat(&bot, &mut redis, chat_id).await?;

        chats.push((chat_id, chat_display_name(&chat).to_owned()));
    }

    let body = ListGroupsTemplate {
        user_name: user.first_name,
        chats,
    }
    .render()
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "manage/telegram_chat.html")]
struct ManageTelegramChatTemplate {
    chat_id: i64,
    chat_name: String,

    options: Vec<ConfigOption>,
}

enum ConfigValue {
    Bool(bool),
}

impl ConfigValue {
    fn bool(&self) -> Option<bool> {
        match self {
            Self::Bool(bool) => Some(*bool),
        }
    }
}

struct ConfigOption {
    key: String,
    name: String,
    help: String,

    value: ConfigValue,
}

#[get("/telegram/chat/{id}", name = "manage_telegram_chat")]
async fn manage_telegram_chat(
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    user: TelegramUser,
    chat_id: web::Path<i64>,
) -> actix_web::Result<HttpResponse, actix_web::Error> {
    let chat_id = chat_id.into_inner();

    let pool = pool.into_inner();

    if !models::ChatAdmin::is_admin(
        &pool,
        models::Chat::Telegram(chat_id),
        models::User::Telegram(user.id),
    )
    .await
    .map_err(actix_web::error::ErrorInternalServerError)?
    .unwrap_or(false)
    {
        return Err(actix_web::error::ErrorUnauthorized("unauthorized"));
    }

    let bot = tgbotapi::Telegram::new(config.telegram_api_key.clone());

    let mut redis = (*redis.into_inner()).clone();
    let chat = get_chat(&bot, &mut redis, chat_id).await?;

    let config = models::Manage::get_chat_config(&pool, models::Chat::Telegram(chat_id))
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    tracing::trace!("discovered config options: {:?}", config);

    let config: HashMap<String, serde_json::Value> = config.into_iter().collect();

    let options = match chat.chat_type {
        tgbotapi::ChatType::Group | tgbotapi::ChatType::Supergroup => {
            vec![
                ConfigOption {
                    key: "group_add".to_string(),
                    name: "Automatic Group Sourcing".to_string(),
                    help: "If the bot should automatically reply with sources to unsourced images."
                        .to_string(),
                    value: config
                        .get("group_add")
                        .and_then(|value| {
                            Some(ConfigValue::Bool(
                                serde_json::from_value(value.to_owned()).ok()?,
                            ))
                        })
                        .unwrap_or(ConfigValue::Bool(false)),
                },
                ConfigOption {
                    key: "group_no_previews".to_string(),
                    name: "Source Link Previews".to_string(),
                    help: "Allow Telegram to show link previews for discovered sources."
                        .to_string(),
                    value: config
                        .get("group_no_previews")
                        .and_then(|value| {
                            Some(ConfigValue::Bool(
                                serde_json::from_value(value.to_owned()).ok()?,
                            ))
                        })
                        .unwrap_or(ConfigValue::Bool(true)),
                },
                ConfigOption {
                    key: "group_no_albums".to_string(),
                    name: "Use Web for Album Sources".to_string(),
                    help: "Send a link to sources for album instead of a message with many links."
                        .to_string(),
                    value: config
                        .get("group_no_albums")
                        .and_then(|value| {
                            Some(ConfigValue::Bool(
                                serde_json::from_value(value.to_owned()).ok()?,
                            ))
                        })
                        .unwrap_or(ConfigValue::Bool(false)),
                },
            ]
        }
        tgbotapi::ChatType::Channel => {
            vec![ConfigOption {
                key: "channel_caption".to_string(),
                name: "Always Use Captions".to_string(),
                help: "If sources should always be set in the caption instead of using buttons."
                    .to_string(),
                value: config
                    .get("channel_caption")
                    .and_then(|value| {
                        Some(ConfigValue::Bool(
                            serde_json::from_value(value.to_owned()).ok()?,
                        ))
                    })
                    .unwrap_or(ConfigValue::Bool(false)),
            }]
        }
        tgbotapi::ChatType::Private => {
            return Err(actix_web::error::ErrorBadRequest(
                "no settings for private chats",
            ))
        }
    };

    let body = ManageTelegramChatTemplate {
        chat_id,
        chat_name: chat_display_name(&chat).to_owned(),
        options,
    }
    .render()
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/telegram/chat/{id}", name = "manage_telegram_chat_post")]
async fn manage_telegram_chat_post(
    request: HttpRequest,
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    user: TelegramUser,
    chat_id: web::Path<i64>,
    form: web::Form<HashMap<String, String>>,
) -> actix_web::Result<HttpResponse, actix_web::Error> {
    let chat_id = chat_id.into_inner();

    let pool = pool.into_inner();

    if !models::ChatAdmin::is_admin(
        &pool,
        models::Chat::Telegram(chat_id),
        models::User::Telegram(user.id),
    )
    .await
    .map_err(actix_web::error::ErrorInternalServerError)?
    .unwrap_or(false)
    {
        return Err(actix_web::error::ErrorUnauthorized("unauthorized"));
    }

    let bot = tgbotapi::Telegram::new(config.telegram_api_key.clone());

    let mut redis = (*redis.into_inner()).clone();
    let chat = get_chat(&bot, &mut redis, chat_id).await?;

    tracing::trace!("form was: {form:?}");

    let mut settings = Vec::new();

    match chat.chat_type {
        tgbotapi::ChatType::Group | tgbotapi::ChatType::Supergroup => {
            settings.push(models::SettingEntry::new(
                "group_add",
                form.get("group_add").map(is_checked).unwrap_or_default(),
            ));

            settings.push(models::SettingEntry::new(
                "group_no_previews",
                form.get("group_no_previews")
                    .map(is_checked)
                    .unwrap_or(true),
            ));

            settings.push(models::SettingEntry::new(
                "group_no_albums",
                form.get("group_no_albums")
                    .map(is_checked)
                    .unwrap_or_default(),
            ));
        }
        tgbotapi::ChatType::Channel => {
            settings.push(models::SettingEntry::new(
                "channel_caption",
                form.get("channel_caption")
                    .map(is_checked)
                    .unwrap_or_default(),
            ));
        }
        tgbotapi::ChatType::Private => {
            return Err(actix_web::error::ErrorBadRequest(
                "no settings for private chats",
            ))
        }
    }

    models::Manage::update_settings(&pool, models::Chat::Telegram(chat.id), &settings)
        .await
        .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Found()
        .insert_header((
            header::LOCATION,
            request
                .url_for("manage_telegram_chat", &[chat_id.to_string()])
                .unwrap()
                .as_str(),
        ))
        .finish())
}

fn is_checked<V: AsRef<str>>(val: V) -> bool {
    val.as_ref().eq_ignore_ascii_case("on")
}
