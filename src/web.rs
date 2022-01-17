use actix_web::{web, App, HttpResponse, HttpServer};
use askama::Template;
use egg_mode::KeyPair;
use serde::Deserialize;
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
    telegram_api_key: String,
    fuzzysearch_api_key: String,
    coconut_secret: String,
    twitter_consumer_key: String,
    twitter_consumer_secret: String,

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

    let bot = tgbotapi::Telegram::new(config.telegram_api_token.clone());

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
    })
    .await
    .expect("could not set telegram bot webhook");

    HttpServer::new(move || {
        let config = Config {
            telegram_api_key: config.telegram_api_token.clone(),
            fuzzysearch_api_key: config.fuzzysearch_api_key.clone(),
            coconut_secret: config.coconut_secret.clone(),
            twitter_consumer_key: config.twitter_consumer_key.clone(),
            twitter_consumer_secret: config.twitter_consumer_secret.clone(),

            cdn_prefix: config.cdn_prefix.clone(),
        };

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .route("/", web::get().to(index))
            .route("/mg/{media_group_id}", web::get().to(media_group))
            .route("/telegram/{secret}", web::post().to(telegram_webhook))
            .route("/fuzzysearch/{secret}", web::post().to(fuzzysearch_webhook))
            .route("/coconut/{secret}", web::post().to(coconut_webhook))
            .route("/twitter/callback", web::get().to(twitter_callback))
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

    let user = models::User::from_one(auth.telegram_id, auth.discord_id).ok_or(Error::Missing)?;

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

#[derive(Deserialize)]
struct CoconutWebHookRequestBody {
    progress: Option<String>,
    output_urls: Option<CoconutWebHookRequestBodyOutputUrls>,
}

#[derive(Deserialize)]
struct CoconutWebHookRequestBodyOutputUrls {
    #[serde(rename = "jpg:250x0")]
    thumbnail: Vec<String>,
    #[serde(rename = "mp4:720p")]
    video_720p: Option<String>,
    #[serde(rename = "mp4:480p")]
    video_480p: Option<String>,
    #[serde(rename = "mp4:360p")]
    video_360p: Option<String>,
}

async fn coconut_webhook(
    config: web::Data<Config>,
    faktory: web::Data<FaktoryClient>,
    secret: web::Path<String>,
    web::Query(query): web::Query<CoconutWebHookRequestQuery>,
    data: web::Json<CoconutWebHookRequestBody>,
) -> Result<HttpResponse, actix_web::Error> {
    tracing::info!("got coconut webhook");

    if config.coconut_secret != secret.into_inner() {
        tracing::warn!("request did not have expected secret");
        return Ok(HttpResponse::Unauthorized().finish());
    }

    let display_name = query.name;

    let job = if let Some(progress) = &data.progress {
        tracing::debug!("event was progress");

        CoconutEventJob::Progress {
            display_name,
            progress: progress.to_string(),
        }
    } else if let Some(output_urls) = &data.output_urls {
        tracing::debug!("event was output");

        let thumb_url = output_urls
            .thumbnail
            .first()
            .ok_or_else(|| actix_web::error::ErrorBadRequest("missing thumbnail"))?
            .to_owned();

        let video_url = if let Some(url) = &output_urls.video_720p {
            url
        } else if let Some(url) = &output_urls.video_480p {
            url
        } else if let Some(url) = &output_urls.video_360p {
            url
        } else {
            return Err(actix_web::error::ErrorBadRequest(
                "all output formats empty",
            ));
        };

        CoconutEventJob::Completed {
            display_name,
            thumb_url,
            video_url: video_url.to_owned(),
        }
    } else {
        return Err(actix_web::error::ErrorBadRequest(
            "no progress and no output urls",
        ));
    };

    tracing::info!("computed event: {:?}", job);

    let job_id = faktory
        .enqueue_job(job, None)
        .await
        .map_err(actix_web::error::ErrorBadRequest)?;

    tracing::trace!(%job_id, "enqueued video event");

    Ok(HttpResponse::Ok().body("OK"))
}
