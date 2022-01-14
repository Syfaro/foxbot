use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use actix_web_httpauth::{
    extractors::AuthenticationError, headers::www_authenticate::basic::Basic,
    middleware::HttpAuthentication,
};
use handlebars::Handlebars;
use serde::Deserialize;
use tgbotapi::Update;

use crate::{Config, HandlerUpdate, ServiceData, UpdateSender};
use foxbot_utils::find_best_photo;

#[derive(Deserialize)]
struct TwitterCallbackRequest {
    denied: Option<String>,
    oauth_token: Option<String>,
    oauth_verifier: Option<String>,
}

#[derive(Deserialize)]
struct FuzzySearchWebHookRequest {
    hash: Option<String>,
}

#[derive(Deserialize)]
struct CoconutWebHookRequestQuery {
    name: String,
}

#[derive(Deserialize)]
struct CoconutWebHookRequestBodyOutputUrls {
    #[serde(rename = "jpg:250x0")]
    thumbnail: Option<Vec<String>>,
    #[serde(rename = "mp4:720p")]
    video_720p: Option<String>,
    #[serde(rename = "mp4:480p")]
    video_480p: Option<String>,
    #[serde(rename = "mp4:360p")]
    video_360p: Option<String>,
}

#[derive(Deserialize)]
struct CoconutWebHookRequestBody {
    progress: Option<String>,
    output_urls: Option<CoconutWebHookRequestBodyOutputUrls>,
}

#[get("/")]
async fn index(hbs: web::Data<Handlebars<'_>>) -> impl Responder {
    let body = hbs.render("home", &None::<()>).unwrap();

    HttpResponse::Ok().body(body)
}

#[get("/health")]
async fn health() -> impl Responder {
    "✓"
}

#[get("/twitter/callback")]
async fn twitter_callback(
    hbs: web::Data<Handlebars<'_>>,
    web::Query(info): web::Query<TwitterCallbackRequest>,
    sender: web::Data<(UpdateSender, UpdateSender)>,
) -> impl Responder {
    let data: Option<()> = None;

    if info.denied.is_some() {
        let body = hbs.render("twitter/denied", &data).unwrap();
        return HttpResponse::Ok().body(body);
    }

    let (oauth_token, oauth_verifier) = match (info.oauth_token, info.oauth_verifier) {
        (Some(oauth_token), Some(oauth_verifier)) => (oauth_token, oauth_verifier),
        _ => {
            let body = hbs.render("400", &data).unwrap();
            return HttpResponse::BadRequest().body(body);
        }
    };

    let update = crate::HandlerUpdate::Service(crate::ServiceData::TwitterVerified {
        token: oauth_token,
        verifier: oauth_verifier,
    });

    sender
        .1
        .send((update, tracing::Span::current()))
        .await
        .unwrap();

    let body = hbs.render("twitter/loggedin", &data).unwrap();
    HttpResponse::Ok().body(body)
}

#[post("/telegram/{secret}")]
async fn telegram_webhook(
    secret: web::Path<(String,)>,
    update: web::Json<Update>,
    config: web::Data<Config>,
    sender: web::Data<(UpdateSender, UpdateSender)>,
) -> impl Responder {
    if secret.into_inner().0 != config.telegram_apitoken {
        return HttpResponse::Forbidden().finish();
    }

    let update: Box<Update> = Box::new(update.into_inner());

    let sender = if update.inline_query.is_some() {
        &sender.0
    } else {
        &sender.1
    };

    sender
        .send((update.into(), tracing::Span::current()))
        .await
        .unwrap();

    HttpResponse::Ok().body("✓")
}

#[post("/fuzzysearch/{secret}")]
async fn fuzzysearch_webhook(
    secret: web::Path<(String,)>,
    hash: web::Json<FuzzySearchWebHookRequest>,
    config: web::Data<Config>,
    sender: web::Data<(UpdateSender, UpdateSender)>,
) -> impl Responder {
    if secret.into_inner().0 != config.fautil_apitoken {
        return HttpResponse::Forbidden().finish();
    }

    let hash = match &hash.hash {
        Some(hash) => hash,
        None => return HttpResponse::Ok().body("No hash"),
    };

    let mut data = [0u8; 8];
    base64::decode_config_slice(&hash, base64::STANDARD, &mut data).unwrap();
    let hash = i64::from_be_bytes(data);

    let service_update = crate::HandlerUpdate::Service(crate::ServiceData::NewHash { hash });

    sender
        .1
        .send((service_update, tracing::Span::current()))
        .await
        .unwrap();

    HttpResponse::Ok().body("✓")
}

#[post("/coconut/{secret}")]
async fn coconut_webhook(
    secret: web::Path<(String,)>,
    data: web::Json<CoconutWebHookRequestBody>,
    web::Query(query): web::Query<CoconutWebHookRequestQuery>,
    config: web::Data<Config>,
    sender: web::Data<(UpdateSender, UpdateSender)>,
) -> impl Responder {
    if secret.into_inner().0 != config.coconut_secret {
        return HttpResponse::Forbidden().finish();
    }

    let display_name = query.name;

    if let Some(progress) = &data.progress {
        let service_update = HandlerUpdate::Service(ServiceData::VideoProgress {
            display_name: display_name.to_owned(),
            progress: progress.to_owned(),
        });

        sender
            .1
            .send((service_update, tracing::Span::current()))
            .await
            .unwrap();

        return HttpResponse::Ok().body("Updated progress");
    }

    let output_urls = match &data.output_urls {
        Some(output_urls) => output_urls,
        None => return HttpResponse::BadRequest().body("No progress and no output URLs"),
    };

    let thumb_url = output_urls.thumbnail.as_ref().unwrap();

    let video_url = if let Some(url) = &output_urls.video_720p {
        url
    } else if let Some(url) = &output_urls.video_480p {
        url
    } else if let Some(url) = &output_urls.video_360p {
        url
    } else {
        return HttpResponse::BadRequest().body("All expected video formats empty");
    };

    let service_update = HandlerUpdate::Service(ServiceData::VideoComplete {
        display_name,
        video_url: video_url.to_owned(),
        thumb_url: thumb_url.first().unwrap().to_owned(),
    });

    sender
        .1
        .send((service_update, tracing::Span::current()))
        .await
        .unwrap();

    HttpResponse::Ok().body("✓")
}

#[get("/metrics")]
async fn metrics() -> impl Responder {
    use prometheus::Encoder;

    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    HttpResponse::Ok().body(buffer)
}

#[derive(serde::Deserialize)]
struct MediaGroupPath {
    media_group_id: String,
}

#[derive(serde::Serialize)]
struct SourceInfo<'a> {
    media_group_id: &'a str,
    message_id: i32,
    file_id: &'a str,
    urls: Vec<String>,
}

#[get("/mg/{media_group_id}")]
async fn mediagroup(
    path: web::Path<MediaGroupPath>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    hbs: web::Data<Handlebars<'_>>,
    config: web::Data<Config>,
) -> impl Responder {
    use foxbot_models::MediaGroup;

    let messages = MediaGroup::get_messages(&conn, &path.media_group_id)
        .await
        .unwrap();

    let mut source_info: Vec<_> = messages
        .iter()
        .map(|message| SourceInfo {
            media_group_id: message.message.media_group_id.as_ref().unwrap(),
            message_id: message.message.message_id,
            file_id: &find_best_photo(message.message.photo.as_deref().unwrap())
                .unwrap()
                .file_id,
            urls: message
                .sources
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|file| file.url())
                .collect(),
        })
        .collect();
    source_info.sort_by(|a, b| a.message_id.cmp(&b.message_id));

    let body = hbs
        .render(
            "mediagroup",
            &serde_json::json!({
                "cdn_url": config.s3_url,
                "cdn_bucket": config.s3_bucket,
                "items": source_info,
            }),
        )
        .unwrap();

    HttpResponse::Ok().body(body)
}

pub async fn serve(
    config: Config,
    high_priority: UpdateSender,
    low_priority: UpdateSender,
    conn: sqlx::Pool<sqlx::Postgres>,
    bot: std::sync::Arc<tgbotapi::Telegram>,
) {
    tracing::info!("starting web server");

    let mut hbs = Handlebars::new();
    hbs.set_strict_mode(true);
    hbs.register_templates_directory(".hbs", "templates/")
        .expect("templates contained bad data");
    let hbs = web::Data::new(hbs);
    let conn = web::Data::new(conn);

    let sender = (high_priority, low_priority);

    let internal_secret: &'static str = Box::leak(config.internal_secret.clone().into_boxed_str());

    HttpServer::new(move || {
        let internal_resources = web::scope("/_")
            .wrap(HttpAuthentication::basic(
                move |req, credentials| async move {
                    if let Some(password) = credentials.password() {
                        if *password == internal_secret {
                            return Ok(req);
                        }
                    }

                    Err(actix_web::Error::from(AuthenticationError::new(
                        Basic::with_realm("FoxBot"),
                    )))
                },
            ))
            .service(health)
            .service(metrics);

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .app_data(hbs.clone())
            .app_data(conn.clone())
            .app_data(web::Data::new(bot.clone()))
            .app_data(web::Data::new(sender.clone()))
            .app_data(web::Data::new(config.clone()))
            .service(telegram_webhook)
            .service(coconut_webhook)
            .service(fuzzysearch_webhook)
            .service(index)
            .service(health)
            .service(twitter_callback)
            .service(mediagroup)
            .service(internal_resources)
    })
    .workers(4)
    .bind("0.0.0.0:8080")
    .unwrap()
    .run()
    .await
    .unwrap();

    tracing::warn!("web server has ended");
}
