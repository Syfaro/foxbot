use std::{collections::HashMap, pin::Pin};

use actix_web::{web, App, HttpResponse, HttpServer};
use askama::Template;
use chrono::TimeZone;
use egg_mode::KeyPair;
use futures::Future;
use hmac::{Hmac, Mac};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    execute::telegram_jobs::{
        CoconutEventJob, NewHashJob, TelegramIngestJob, TwitterAccountAddedJob,
    },
    models,
    services::faktory::FaktoryClient,
    utils, Error, WebConfig,
};

struct Config {
    public_endpoint: String,

    telegram_api_key: String,
    telegram_bot_username: String,
    fuzzysearch_api_key: String,
    coconut_secret: String,
    twitter_consumer_key: String,
    twitter_consumer_secret: String,
    manage_jwt_secret: Vec<u8>,

    feedback_base: String,
    fider_oauth_url: String,
    jwt_secret: String,

    cdn_prefix: String,
}

pub async fn web(config: WebConfig) {
    tracing::info!("starting server to listen for web requests");

    let faktory = FaktoryClient::connect(config.faktory_url)
        .await
        .expect("could not connect to faktory");

    let pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("could not connect to database");

    let redis = redis::Client::open(config.redis_url).expect("could not connect to redis");
    let redis = redis::aio::ConnectionManager::new(redis)
        .await
        .expect("could not create redis connection manager");

    crate::run_migrations(&pool).await;

    let bot = tgbotapi::Telegram::new(config.telegram_api_token.clone());

    let me = bot
        .make_request(&tgbotapi::requests::GetMe)
        .await
        .expect("could not get bot information");

    bot.make_request(&tgbotapi::requests::SetWebhook {
        url: format!(
            "{}/telegram/{}",
            config.public_endpoint, config.telegram_api_token
        ),
        allowed_updates: Some(vec![
            "message".into(),
            "channel_post".into(),
            "inline_query".into(),
            "chosen_inline_result".into(),
            "callback_query".into(),
            "my_chat_member".into(),
            "chat_member".into(),
        ]),
        ..Default::default()
    })
    .await
    .expect("could not set telegram bot webhook");

    let manage_jwt_secret = hex::decode(&config.jwt_secret).expect("manage jwt secret was not hex");

    let session_secret = actix_web::cookie::Key::from(
        &hex::decode(config.session_secret).expect("cookie secret was not hex"),
    );

    HttpServer::new(move || {
        let config = Config {
            public_endpoint: config.public_endpoint.clone(),

            telegram_api_key: config.telegram_api_token.clone(),
            telegram_bot_username: me.username.clone().expect("bot had no username"),
            fuzzysearch_api_key: config.fuzzysearch_api_key.clone(),
            coconut_secret: config.coconut_secret.clone(),
            twitter_consumer_key: config.twitter_consumer_key.clone(),
            twitter_consumer_secret: config.twitter_consumer_secret.clone(),
            manage_jwt_secret: manage_jwt_secret.clone(),

            feedback_base: config.feedback_base.clone(),
            fider_oauth_url: config.fider_oauth_url.clone(),
            jwt_secret: config.jwt_secret.clone(),

            cdn_prefix: config.cdn_prefix.clone(),
        };

        let session = actix_session::SessionMiddleware::builder(
            actix_session::storage::CookieSessionStore::default(),
            session_secret.clone(),
        )
        .cookie_name("foxbot".to_string())
        .build();

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .wrap(session)
            .route("/", web::get().to(index))
            .route("/mg/{media_group_id}", web::get().to(media_group))
            .route("/telegram/{secret}", web::post().to(telegram_webhook))
            .route("/fuzzysearch/{secret}", web::post().to(fuzzysearch_webhook))
            .route("/coconut/{secret}", web::post().to(coconut_webhook))
            .route("/twitter/callback", web::get().to(twitter_callback))
            .route("/feedback", web::get().to(feedback_redirect))
            .route("/feedback/authorize", web::get().to(feedback_authorize))
            .route("/feedback/token", web::post().to(feedback_token))
            .service(
                web::scope("/manage")
                    .route("/login", web::get().to(manage_login))
                    .route("/list", web::get().to(manage_list))
                    .route("/telegram/chat/{id}", web::get().to(manage_telegram_chat))
                    .route(
                        "/telegram/chat/{id}",
                        web::post().to(manage_telegram_chat_post),
                    ),
            )
            .app_data(web::Data::new(faktory.clone()))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(redis.clone()))
            .app_data(web::Data::new(config))
    })
    .bind(config.http_host)
    .expect("could not bind to http_host")
    .run()
    .await
    .expect("could not complete server");
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate;

async fn index() -> Result<HttpResponse, actix_web::Error> {
    let body = IndexTemplate
        .render()
        .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().body(body))
}

#[derive(Template)]
#[template(path = "media_group.html")]
struct MediaGroupTemplate<'a> {
    cdn_prefix: &'a str,
    items: &'a [SourceInfo<'a>],
}

struct SourceInfo<'a> {
    media_group_id: &'a str,
    file_id: &'a str,
    urls: Vec<String>,
}

async fn media_group(
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    media_group_id: web::Path<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let media_group_items =
        models::MediaGroup::get_messages(pool.get_ref(), media_group_id.as_str())
            .await
            .map_err(actix_web::error::ErrorInternalServerError)?;

    let source_info: Vec<_> = media_group_items
        .iter()
        .flat_map(|media_group_item| {
            let best_photo = utils::find_best_photo(media_group_item.message.photo.as_deref()?)?;

            let urls = media_group_item
                .sources
                .as_ref()
                .map(|sources| sources.0.iter().map(|source| source.url()).collect())
                .unwrap_or_default();

            Some(SourceInfo {
                media_group_id: &media_group_item.media_group_id,
                file_id: &best_photo.file_id,
                urls,
            })
        })
        .collect();

    let body = MediaGroupTemplate {
        cdn_prefix: &config.cdn_prefix,
        items: &source_info,
    }
    .render()
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().body(body))
}

#[derive(Deserialize)]
struct TwitterCallbackRequest {
    oauth_token: Option<String>,
    oauth_verifier: Option<String>,
}

#[derive(Template)]
#[template(path = "twitter.html")]
struct TwitterCalbackTemplate {
    successful: bool,
    username: Option<String>,
}

async fn twitter_callback(
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    web::Query(info): web::Query<TwitterCallbackRequest>,
    faktory: web::Data<FaktoryClient>,
) -> Result<HttpResponse, actix_web::Error> {
    let tokens = (info.oauth_token, info.oauth_verifier);

    let (successful, username) = if let (Some(oauth_token), Some(oauth_verifier)) = tokens {
        if let Some((_account_id, user, username)) =
            verify_twitter_account(&config, &pool, oauth_token, oauth_verifier)
                .await
                .map_err(actix_web::error::ErrorInternalServerError)?
        {
            let job = TwitterAccountAddedJob {
                telegram_id: user.telegram_id(),
                twitter_username: username.clone(),
            };

            faktory
                .enqueue_job(job, None)
                .await
                .map_err(actix_web::error::ErrorInternalServerError)?;

            (true, Some(username))
        } else {
            (true, None)
        }
    } else {
        (false, None)
    };

    let body = TwitterCalbackTemplate {
        successful,
        username,
    }
    .render()
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().body(body))
}

async fn verify_twitter_account(
    config: &Config,
    pool: &PgPool,
    oauth_token: String,
    oauth_verifier: String,
) -> Result<Option<(i32, models::User, String)>, Error> {
    let auth = match models::TwitterAuth::get_request(pool, &oauth_token).await? {
        Some(auth) => auth,
        None => return Ok(None),
    };

    let user = models::User::from_one(auth.telegram_id, auth.discord_id)
        .ok_or_else(|| Error::missing("user for twitter"))?;

    let con_token = KeyPair::new(
        config.twitter_consumer_key.clone(),
        config.twitter_consumer_secret.clone(),
    );
    let request_token = KeyPair::new(auth.request_key, auth.request_secret);

    let (token, _user_id, user_username) =
        egg_mode::auth::access_token(con_token, &request_token, oauth_verifier).await?;

    let access = match token {
        egg_mode::Token::Access { access, .. } => access,
        _ => unreachable!("token should always be access"),
    };

    let twitter_account =
        models::TwitterAccount::save_authorization(pool, user.clone(), access).await?;

    Ok(Some((twitter_account.id, user, user_username)))
}

async fn telegram_webhook(
    config: web::Data<Config>,
    faktory: web::Data<FaktoryClient>,
    secret: web::Path<String>,
    body: web::Json<serde_json::Value>,
) -> Result<HttpResponse, actix_web::Error> {
    tracing::info!("got telegram webhook");

    if config.telegram_api_key != secret.into_inner() {
        tracing::warn!("request did not have expected secret");
        return Ok(HttpResponse::Unauthorized().finish());
    }

    let value = body.into_inner();

    let job = TelegramIngestJob { update: value };

    let is_high_priority_update = job.is_high_priority();

    tracing::debug!(
        is_high_priority_update,
        "checked if update was high priority"
    );

    let job_id = faktory
        .enqueue_job(job, None)
        .await
        .map_err(actix_web::error::ErrorBadRequest)?;

    tracing::trace!(%job_id, "enqueued message");

    Ok(HttpResponse::Ok().body("OK"))
}

#[derive(Deserialize)]
struct FuzzySearchWebHookRequest {
    hash: Option<String>,
}

async fn fuzzysearch_webhook(
    config: web::Data<Config>,
    faktory: web::Data<FaktoryClient>,
    secret: web::Path<String>,
    body: web::Json<FuzzySearchWebHookRequest>,
) -> Result<HttpResponse, actix_web::Error> {
    tracing::info!("got fuzzysearch webhook");

    if config.fuzzysearch_api_key != secret.into_inner() {
        tracing::warn!("request did not have expected secret");
        return Ok(HttpResponse::Unauthorized().finish());
    }

    let hash = match &body.hash {
        Some(hash) => hash,
        None => return Ok(HttpResponse::Ok().body("no hash")),
    };

    let mut data = [0u8; 8];
    base64::decode_config_slice(&hash, base64::STANDARD, &mut data).unwrap();

    let job = NewHashJob { hash: data };

    let job_id = faktory
        .enqueue_job(job, None)
        .await
        .map_err(actix_web::error::ErrorBadRequest)?;

    tracing::trace!(%job_id, "enqueued new hash");

    Ok(HttpResponse::Ok().body("OK"))
}

#[derive(Deserialize)]
struct CoconutWebHookRequestQuery {
    name: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(tag = "event", content = "data")]
enum CoconutWebHook {
    #[serde(rename = "input.transferred")]
    InputTransferred { progress: String },
    #[serde(rename = "output.completed")]
    OutputCompleted { progress: String },
    #[serde(rename = "job.completed")]
    JobCompleted { outputs: Vec<CoconutOutput> },
}

#[derive(Deserialize)]
struct CoconutOutput {
    key: String,
    #[serde(flatten)]
    status: CoconutOutputStatus,
}

#[derive(Deserialize)]
#[serde(tag = "status")]
enum CoconutOutputStatus {
    #[serde(rename = "video.encoded")]
    VideoEncoded {
        #[serde(flatten)]
        file: CoconutOutputFile,
    },
    #[serde(rename = "video.skipped")]
    VideoSkipped,
    #[serde(rename = "image.created")]
    ImageCreated {
        #[serde(flatten)]
        file: CoconutOutputFile,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum CoconutOutputFile {
    Image {
        urls: Vec<String>,
    },
    Video {
        url: String,
        metadata: CoconutMetadata,
    },
}

#[derive(Deserialize)]
struct CoconutMetadata {
    streams: Vec<CoconutMetadataStream>,
    format: CoconutMetadataFormat,
}

#[derive(Deserialize)]
struct CoconutMetadataStream {
    codec_name: Option<CoconutCodecType>,
    #[serde(flatten)]
    dimensions: Option<CoconutDimensions>,
}

#[derive(Deserialize)]
struct CoconutDimensions {
    width: i32,
    height: i32,
}

#[derive(Deserialize)]
enum CoconutCodecType {
    #[serde(rename = "h264")]
    H264,
    #[serde(rename = "vp9")]
    Vp9,
    #[serde(rename = "aac")]
    Aac,
}

#[derive(Deserialize)]
struct CoconutMetadataFormat {
    size: String,
    duration: String,
}

const COCONUT_FORMATS: &[&str] = &["mp4:1080p", "mp4:720p", "mp4:480p", "mp4:360p"];

async fn coconut_webhook(
    config: web::Data<Config>,
    faktory: web::Data<FaktoryClient>,
    secret: web::Path<String>,
    web::Query(query): web::Query<CoconutWebHookRequestQuery>,
    data: web::Json<serde_json::Value>,
) -> Result<HttpResponse, actix_web::Error> {
    tracing::info!("got coconut webhook");

    if config.coconut_secret != secret.into_inner() {
        tracing::warn!("request did not have expected secret");
        return Ok(HttpResponse::Unauthorized().finish());
    }

    let event: CoconutWebHook = match serde_json::from_value(data.into_inner()) {
        Ok(data) => data,
        Err(err) => {
            tracing::error!("unknown coconut data: {}", err);
            return Ok(HttpResponse::Ok().body("OK"));
        }
    };

    let display_name = query.name;

    let job = match event {
        CoconutWebHook::OutputCompleted { progress, .. } => Some(CoconutEventJob::Progress {
            display_name,
            progress,
        }),
        CoconutWebHook::JobCompleted { outputs, .. } => {
            let thumb_url = outputs
                .iter()
                .find_map(|output| match &output.status {
                    CoconutOutputStatus::ImageCreated {
                        file: CoconutOutputFile::Image { urls },
                    } => urls.first(),
                    _ => None,
                })
                .ok_or_else(|| actix_web::error::ErrorBadRequest("missing thumbnail"))?
                .to_owned();

            let mut video_formats: HashMap<
                String,
                (String, String, String, Vec<CoconutMetadataStream>),
            > = outputs
                .into_iter()
                .filter_map(|output| match output.status {
                    CoconutOutputStatus::VideoEncoded {
                        file: CoconutOutputFile::Video { url, metadata },
                    } => Some((
                        output.key,
                        (
                            url,
                            metadata.format.size,
                            metadata.format.duration,
                            metadata.streams,
                        ),
                    )),
                    _ => None,
                })
                .collect();

            let mut video_data = None;

            for format in COCONUT_FORMATS {
                if let Some(data) = video_formats.remove(*format) {
                    video_data = Some(data);
                    break;
                }
            }

            let (video_url, video_size, video_duration, streams) = video_data
                .ok_or_else(|| actix_web::error::ErrorBadRequest("no known video formats"))?;

            let video_size = video_size.parse().unwrap_or(i64::MAX);

            let duration = video_duration.parse::<f32>().unwrap_or_default().ceil() as i32;

            let dimensions = streams
                .into_iter()
                .find_map(|stream| match stream.codec_name {
                    Some(CoconutCodecType::H264) => stream.dimensions,
                    _ => None,
                })
                .ok_or_else(|| actix_web::error::ErrorBadRequest("no known streams"))?;

            Some(CoconutEventJob::Completed {
                display_name,
                thumb_url,
                video_url,
                video_size,
                duration,
                height: dimensions.height,
                width: dimensions.width,
            })
        }
        CoconutWebHook::InputTransferred { .. } => None,
    };

    tracing::info!("computed event: {:?}", job);

    if let Some(job) = job {
        let job_id = faktory
            .enqueue_job(job, None)
            .await
            .map_err(actix_web::error::ErrorBadRequest)?;

        tracing::trace!(%job_id, "enqueued video event");
    } else {
        tracing::debug!("event was not actionable");
    }

    Ok(HttpResponse::Ok().body("OK"))
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

async fn manage_login(
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
        .ok_or(actix_web::error::ErrorBadRequest("missing id"))?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let first_name = query
        .get("first_name")
        .ok_or(actix_web::error::ErrorBadRequest("missing first_name"))?
        .to_owned();

    let auth_date: i64 = query
        .get("auth_date")
        .ok_or(actix_web::error::ErrorBadRequest("missing auth_date"))?
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
        .insert_header((actix_web::http::header::LOCATION, "/manage/list"))
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

    let mut options = match chat.chat_type {
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
                        .unwrap_or_else(|| ConfigValue::Bool(false)),
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
                        .unwrap_or_else(|| ConfigValue::Bool(true)),
                },
                ConfigOption {
                    key: "group_no_albums".to_string(),
                    name: "Disable Inline Album Sources".to_string(),
                    help: "Send a link to sources for album instead of a message with many links."
                        .to_string(),
                    value: config
                        .get("group_no_albums")
                        .and_then(|value| {
                            Some(ConfigValue::Bool(
                                serde_json::from_value(value.to_owned()).ok()?,
                            ))
                        })
                        .unwrap_or_else(|| ConfigValue::Bool(false)),
                },
            ]
        }
        tgbotapi::ChatType::Channel => {
            vec![ConfigOption {
                key: "channel_caption".to_string(),
                name: "Always Use Captions".to_string(),
                help: "If sources should always be set in the caption instead of using buttons."
                    .to_string(),
                value: ConfigValue::Bool(false),
            }]
        }
        tgbotapi::ChatType::Private => {
            return Err(actix_web::error::ErrorBadRequest(
                "no settings for private chats",
            ))
        }
    };

    options.push(ConfigOption {
        key: "nsfw".to_string(),
        name: "Disallow NSFW Content".to_string(),
        help: "If the bot is should skip sending NSFW links.".to_string(),
        value: ConfigValue::Bool(false),
    });

    let body = ManageTelegramChatTemplate {
        chat_id,
        chat_name: chat_display_name(&chat).to_owned(),
        options,
    }
    .render()
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

async fn manage_telegram_chat_post(
    config: web::Data<Config>,
    pool: web::Data<PgPool>,
    user: TelegramUser,
    chat_id: web::Path<i64>,
    form: web::Form<HashMap<String, String>>,
) -> actix_web::Result<HttpResponse, actix_web::Error> {
    tracing::info!("form: {:?}", form);

    Ok(HttpResponse::Ok().body("OK"))
}

async fn feedback_redirect(
    config: web::Data<Config>,
    web::Query(mut query): web::Query<HashMap<String, String>>,
    session: actix_session::Session,
) -> Result<HttpResponse, actix_web::Error> {
    let hash = query
        .remove("hash")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing hash"))?;

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
        return Err(actix_web::error::ErrorBadRequest("data too old"));
    }

    let mut data: Vec<_> = query.into_iter().collect();
    data.sort_by(|(a, _), (b, _)| a.cmp(b));

    let data = data
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join("\n");

    let token = Sha256::digest(&config.telegram_api_key);

    let mut mac = Hmac::<Sha256>::new_from_slice(&token)
        .expect("hmac could not be constructed with provided token");
    mac.update(data.as_bytes());

    mac.verify_slice(&hash)
        .map_err(actix_web::error::ErrorBadRequest)?;

    session.insert("telegram-login", (id, first_name))?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", config.fider_oauth_url.clone()))
        .finish())
}

#[derive(Debug, Deserialize)]
struct FiderOAuthQuery {
    redirect_uri: String,
    state: String,
}

#[derive(Serialize, Deserialize)]
struct FiderCodeData {
    telegram_id: i64,
    telegram_first_name: String,
    exp: i64,
}

#[derive(Template)]
#[template(path = "signin.html")]
struct SignInTemplate<'a> {
    bot_username: &'a str,
    auth_url: &'a str,
}

async fn feedback_authorize(
    config: web::Data<Config>,
    session: actix_session::Session,
    web::Query(query): web::Query<FiderOAuthQuery>,
) -> Result<HttpResponse, actix_web::Error> {
    let login_data: (i64, String) = match session.get("telegram-login") {
        Ok(Some(id)) => id,
        _ => {
            let auth_url = format!("{}/feedback", config.public_endpoint);

            return SignInTemplate {
                bot_username: &config.telegram_bot_username,
                auth_url: &auth_url,
            }
            .render()
            .map(|body| {
                HttpResponse::Ok()
                    .insert_header(("content-type", "text/html"))
                    .body(body)
            })
            .map_err(actix_web::error::ErrorInternalServerError);
        }
    };

    if !query
        .redirect_uri
        .to_ascii_lowercase()
        .starts_with(&config.feedback_base.to_ascii_lowercase())
    {
        return Err(actix_web::error::ErrorBadRequest("redirect uri not valid"));
    }

    let code = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &FiderCodeData {
            telegram_id: login_data.0,
            telegram_first_name: login_data.1,
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        },
        &jsonwebtoken::EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(actix_web::error::ErrorInternalServerError)?;

    let mut location =
        url::Url::parse(&query.redirect_uri).map_err(actix_web::error::ErrorBadRequest)?;

    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &query.state);

    Ok(HttpResponse::Found()
        .insert_header(("Location", location.as_str()))
        .finish())
}

#[derive(Deserialize)]
struct FiderOAuthToken {
    code: String,
}

async fn feedback_token(
    config: web::Data<Config>,
    form: web::Form<FiderOAuthToken>,
    req: actix_web::HttpRequest,
) -> Result<HttpResponse, actix_web::Error> {
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|auth| auth.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Basic "))
        .and_then(|auth| base64::decode(auth).ok())
        .and_then(|auth| String::from_utf8(auth).ok())
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing authorization"))?;

    match auth.split_once(':') {
        Some((username, password)) if username == "foxbot" && password == config.jwt_secret => (),
        _ => return Err(actix_web::error::ErrorUnauthorized("bad authorization")),
    }

    jsonwebtoken::decode::<FiderCodeData>(
        &form.code,
        &jsonwebtoken::DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
    )
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok()
        .append_header(("content-type", "application/json"))
        .body(
            serde_json::to_string(&serde_json::json!({
                "access_token": form.code,
                "token_type": "Bearer",
            }))
            .map_err(actix_web::error::ErrorInternalServerError)?,
        ))
}
