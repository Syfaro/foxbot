use std::collections::HashMap;

use actix_web::{web, App, HttpResponse, HttpServer};
use askama::Template;
use chrono::TimeZone;
use egg_mode::KeyPair;
use hmac::{Hmac, Mac};
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

    let session_secret = actix_web::cookie::Key::from(config.session_secret.as_bytes());

    HttpServer::new(move || {
        let config = Config {
            public_endpoint: config.public_endpoint.clone(),

            telegram_api_key: config.telegram_api_token.clone(),
            telegram_bot_username: me.username.clone().expect("bot had no username"),
            fuzzysearch_api_key: config.fuzzysearch_api_key.clone(),
            coconut_secret: config.coconut_secret.clone(),
            twitter_consumer_key: config.twitter_consumer_key.clone(),
            twitter_consumer_secret: config.twitter_consumer_secret.clone(),

            feedback_base: config.feedback_base.clone(),
            fider_oauth_url: config.fider_oauth_url.clone(),
            jwt_secret: config.jwt_secret.clone(),

            cdn_prefix: config.cdn_prefix.clone(),
        };

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .wrap(
                actix_session::SessionMiddleware::builder(
                    actix_session::storage::CookieSessionStore::default(),
                    session_secret.clone(),
                )
                .cookie_name("foxbot-session".to_owned())
                .build(),
            )
            .route("/", web::get().to(index))
            .route("/mg/{media_group_id}", web::get().to(media_group))
            .route("/telegram/{secret}", web::post().to(telegram_webhook))
            .route("/fuzzysearch/{secret}", web::post().to(fuzzysearch_webhook))
            .route("/coconut/{secret}", web::post().to(coconut_webhook))
            .route("/twitter/callback", web::get().to(twitter_callback))
            .route("/feedback", web::get().to(feedback_redirect))
            .route("/feedback/authorize", web::get().to(feedback_authorize))
            .route("/feedback/token", web::post().to(feedback_token))
            .app_data(web::Data::new(faktory.clone()))
            .app_data(web::Data::new(pool.clone()))
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
        return Err(actix_web::error::ErrorBadRequest("data too old").into());
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
