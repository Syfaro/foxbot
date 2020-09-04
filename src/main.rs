#![feature(try_trait)]

use sentry::integrations::anyhow::capture_anyhow;
use sites::{PostInfo, Site};
use std::collections::HashMap;
use std::sync::Arc;
use tgbotapi::{requests::*, *};
use tokio::sync::{Mutex, RwLock};
use tracing_futures::Instrument;
use unic_langid::LanguageIdentifier;

mod coconut;
mod handlers;
mod migrations;
pub mod models;
mod sites;
mod utils;

// MARK: Statics and types

type BoxedSite = Box<dyn Site + Send + Sync>;
type BoxedHandler = Box<dyn handlers::Handler + Send + Sync>;

/// Generates a random 24 character alphanumeric string.
///
/// Not cryptographically secure but unique enough for Telegram's unique IDs.
fn generate_id() -> String {
    use rand::Rng;
    let rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .collect()
}

/// Artwork used for examples throughout the bot.
static STARTING_ARTWORK: &[&str] = &[
    "https://www.furaffinity.net/view/33742297/",
    "https://www.furaffinity.net/view/33166216/",
    "https://www.furaffinity.net/view/33040454/",
    "https://www.furaffinity.net/view/32914936/",
    "https://www.furaffinity.net/view/32396231/",
    "https://www.furaffinity.net/view/32267612/",
    "https://www.furaffinity.net/view/32232169/",
];

static L10N_RESOURCES: &[&str] = &["foxbot.ftl"];
static L10N_LANGS: &[&str] = &["en-US"];

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Config {
    // Site config
    pub fa_a: String,
    pub fa_b: String,
    pub weasyl_apitoken: String,
    pub inkbunny_username: String,
    pub inkbunny_password: String,

    // Twitter config
    pub twitter_consumer_key: String,
    pub twitter_consumer_secret: String,

    // InfluxDB config
    influx_host: String,
    influx_db: String,
    influx_user: String,
    influx_pass: String,

    // Logging
    jaeger_collector: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_organization_slug: Option<String>,
    pub sentry_project_slug: Option<String>,

    // Telegram config
    telegram_apitoken: String,
    pub use_webhooks: Option<bool>,
    pub webhook_endpoint: Option<String>,
    pub http_host: Option<String>,
    http_secret: Option<String>,

    // Video handling
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_token: String,
    pub s3_secret: String,
    pub s3_bucket: String,
    pub s3_url: String,

    pub fautil_apitoken: String,

    pub coconut_apitoken: String,
    pub coconut_webhook: String,

    pub size_images: Option<bool>,
    pub cache_images: Option<bool>,

    // SQLite database
    #[cfg(feature = "sqlite")]
    pub database: String,

    // Postgres database
    #[cfg(feature = "postgres")]
    pub db_host: String,
    #[cfg(feature = "postgres")]
    pub db_user: String,
    #[cfg(feature = "postgres")]
    pub db_pass: String,
    #[cfg(feature = "postgres")]
    pub db_name: String,
}

// MARK: Initialization

/// Configure tracing with Jaeger.
fn configure_tracing(collector: String) {
    use opentelemetry::{
        api::{KeyValue, Provider},
        sdk::{Config as TelemConfig, Sampler},
    };
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::prelude::*;

    let env = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let fmt_layer = tracing_subscriber::fmt::layer();
    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .finish();
    let registry = tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer);

    let exporter = opentelemetry_jaeger::Exporter::builder()
        .with_agent_endpoint(collector)
        .with_process(opentelemetry_jaeger::Process {
            service_name: "foxbot".to_string(),
            tags: vec![
                KeyValue::new("environment", env),
                KeyValue::new("version", env!("CARGO_PKG_VERSION")),
            ],
        })
        .init()
        .expect("Unable to create jaeger exporter");

    let provider = opentelemetry::sdk::Provider::builder()
        .with_simple_exporter(exporter)
        .with_config(TelemConfig {
            default_sampler: Box::new(Sampler::Always),
            ..Default::default()
        })
        .build();

    opentelemetry::global::set_provider(provider);

    let tracer = opentelemetry::global::trace_provider().get_tracer("foxbot");
    let telem_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let registry = registry.with(telem_layer);

    registry.init();
}

#[cfg(feature = "sqlite")]
async fn run_migrations(database: &str) {
    let mut conn = rusqlite::Connection::open(database).expect("Unable to open database");

    migrations::runner()
        .run(&mut conn)
        .expect("Unable to migrate database");
}

#[cfg(feature = "postgres")]
async fn run_migrations(host: &str, user: &str, pass: &str, name: &str) {
    let dsn = format!(
        "host={} user={} password={} dbname={}",
        host, user, pass, name
    );

    let (mut client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .expect("Unable to connect to database");

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!("error in tokio-postgres migrations runner: {:?}", e);
        }
    });

    migrations::runner()
        .run_async(&mut client)
        .await
        .expect("Unable to migrate database");
}

#[cfg(feature = "sqlite")]
async fn setup_db(config: &Config) -> String {
    run_migrations(&config.database).await;
    format!("file:{}", config.database)
}

#[cfg(feature = "postgres")]
async fn setup_db(config: &Config) -> String {
    run_migrations(
        &config.db_host,
        &config.db_user,
        &config.db_pass,
        &config.db_name,
    )
    .await;
    format!(
        "postgres://{}:{}@{}/{}",
        config.db_user, config.db_pass, config.db_host, config.db_name
    )
}

#[tokio::main]
async fn main() {
    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let jaeger_collector = match &config.jaeger_collector {
        Some(collector) => collector.to_owned(),
        _ => panic!("Missing JAEGER_COLLECTOR"),
    };

    configure_tracing(jaeger_collector);

    let url = setup_db(&config).await;

    let pool = quaint::pooled::Quaint::builder(&url)
        .expect("Unable to connect to database")
        .build();

    let fapi = Arc::new(fuzzysearch::FuzzySearch::new(
        config.fautil_apitoken.clone(),
    ));

    let sites: Vec<BoxedSite> = vec![
        Box::new(sites::E621::new()),
        Box::new(sites::FurAffinity::new(
            (config.fa_a.clone(), config.fa_b.clone()),
            config.fautil_apitoken.clone(),
        )),
        Box::new(sites::Weasyl::new(config.weasyl_apitoken.clone())),
        Box::new(
            sites::Twitter::new(
                config.twitter_consumer_key.clone(),
                config.twitter_consumer_secret.clone(),
                pool.clone(),
            )
            .await,
        ),
        Box::new(sites::Inkbunny::new(
            config.inkbunny_username.clone(),
            config.inkbunny_password.clone(),
        )),
        Box::new(sites::Mastodon::new()),
        Box::new(sites::Direct::new(fapi.clone())),
    ];

    let bot = Arc::new(Telegram::new(config.telegram_apitoken.clone()));

    let influx = influxdb::Client::new(config.influx_host.clone(), config.influx_db.clone())
        .with_auth(config.influx_user.clone(), config.influx_pass.clone());

    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);

    let mut dir = std::env::current_dir().expect("Unable to get directory");
    dir.push("langs");

    let mut langs = HashMap::new();

    for lang in L10N_LANGS {
        let path = dir.join(lang);

        let mut lang_resources = Vec::with_capacity(L10N_RESOURCES.len());
        let langid = lang
            .parse::<LanguageIdentifier>()
            .expect("Unable to parse language");

        for resource in L10N_RESOURCES {
            let file = path.join(resource);
            let s = std::fs::read_to_string(file).expect("Unable to read file");

            lang_resources.push(s);
        }

        langs.insert(langid, lang_resources);
    }

    let bot_user = bot
        .make_request(&GetMe)
        .await
        .expect("Unable to fetch bot user");

    let handlers: Vec<BoxedHandler> = vec![
        Box::new(handlers::InlineHandler),
        Box::new(handlers::ChosenInlineHandler),
        Box::new(handlers::ChannelPhotoHandler),
        Box::new(handlers::GroupAddHandler),
        Box::new(handlers::PhotoHandler),
        Box::new(handlers::CommandHandler),
        Box::new(handlers::GroupSourceHandler),
        Box::new(handlers::TextHandler),
        Box::new(handlers::ErrorReplyHandler::new()),
        Box::new(handlers::SettingsHandler),
        Box::new(handlers::ErrorCleanup),
    ];

    let region = rusoto_core::Region::Custom {
        name: config.s3_region.clone(),
        endpoint: config.s3_endpoint.clone(),
    };

    let client = rusoto_core::request::HttpClient::new().unwrap();
    let provider = rusoto_credential::StaticProvider::new_minimal(
        config.s3_token.clone(),
        config.s3_secret.clone(),
    );
    let s3 = rusoto_s3::S3Client::new_with(client, provider, region);

    let coconut = coconut::Coconut::new(
        config.coconut_apitoken.clone(),
        config.coconut_webhook.clone(),
        config.s3_endpoint.clone(),
        config.s3_token.clone(),
        config.s3_secret.clone(),
        config.s3_bucket.clone(),
    );

    let handler = Arc::new(MessageHandler {
        bot_user,
        langs,
        best_lang: RwLock::new(HashMap::new()),
        handlers,
        config: config.clone(),

        bot: bot.clone(),
        fapi,
        influx: Arc::new(influx),
        finder,
        s3,
        coconut,

        sites: Mutex::new(sites),
        conn: pool,
    });

    let _guard = if let Some(dsn) = config.sentry_dsn {
        Some(sentry::init(sentry::ClientOptions {
            dsn: Some(dsn.parse().unwrap()),
            debug: true,
            release: option_env!("RELEASE").map(std::borrow::Cow::from),
            attach_stacktrace: true,
            ..Default::default()
        }))
    } else {
        None
    };

    tracing::info!(
        "sentry enabled: {}",
        _guard
            .as_ref()
            .map(|sentry| sentry.is_enabled())
            .unwrap_or(false)
    );

    let use_webhooks = match config.use_webhooks {
        Some(use_webhooks) if use_webhooks => true,
        _ => false,
    };

    if use_webhooks {
        let webhook_endpoint = config.webhook_endpoint.expect("Missing WEBHOOK_ENDPOINT");
        let set_webhook = SetWebhook {
            url: webhook_endpoint,
        };
        if let Err(e) = bot.make_request(&set_webhook).await {
            panic!(e);
        }
        receive_webhook(
            config.http_host.expect("Missing HTTP_HOST"),
            config.http_secret.expect("Missing HTTP_SECRET"),
            handler,
        )
        .await;
    } else {
        let delete_webhook = DeleteWebhook;
        if let Err(e) = bot.make_request(&delete_webhook).await {
            panic!(e);
        }
        poll_updates(bot, handler).await;
    }
}

// MARK: Get Bot API updates

/// Handle an incoming HTTP POST request to /{token}.
///
/// It spawns a handler for each request.
#[tracing::instrument(skip(req, handler, secret))]
async fn handle_request(
    req: hyper::Request<hyper::Body>,
    count: Arc<std::sync::atomic::AtomicUsize>,
    handler: Arc<MessageHandler>,
    secret: &str,
) -> hyper::Result<hyper::Response<hyper::Body>> {
    use hyper::{Body, Response, StatusCode};

    let now = std::time::Instant::now();

    tracing::trace!(method = ?req.method(), path = req.uri().path(), "got HTTP request");

    let path = req.uri().path();
    let uri = req.uri().clone();

    match (req.method(), path) {
        (&hyper::Method::POST, path) if path == secret => {
            tracing::trace!("handling update");
            let body = req.into_body();
            let bytes = hyper::body::to_bytes(body)
                .await
                .expect("unable to read body bytes");

            let update: Update = match serde_json::from_slice(&bytes) {
                Ok(update) => update,
                Err(err) => {
                    tracing::error!("error decoding incoming json: {:?}", err);
                    let mut resp = Response::default();
                    *resp.status_mut() = StatusCode::BAD_REQUEST;
                    return Ok(resp);
                }
            };

            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let handler_clone = handler.clone();
            tokio::spawn(
                async move {
                    tracing::debug!("got update: {:?}", update);
                    handler_clone.handle_update(update).await;
                    count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    tracing::debug!("finished handling message");
                }
                .in_current_span(),
            );

            let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "http")
                .add_field("duration", now.elapsed().as_millis() as i64);

            if let Err(e) = handler.influx.query(&point).await {
                capture_anyhow(&anyhow::anyhow!("InfluxDB error: {:?}", e));
                tracing::error!("unable to send http request info to InfluxDB: {:?}", e);
            }

            Ok(Response::new(Body::from("✓")))
        }
        (&hyper::Method::GET, "/health") => Ok(Response::new(Body::from("✓"))),
        (&hyper::Method::POST, "/video") => {
            let body = req.into_body();
            let bytes = hyper::body::to_bytes(body)
                .await
                .expect("unable to read body bytes");

            tracing::debug!("Got video body: {:?}", bytes);

            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let handler_clone = handler.clone();
            tokio::spawn(
                async move {
                    let update: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    let url = update
                        .get("output_urls")
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .get("mp4:720p")
                        .unwrap()
                        .as_str()
                        .unwrap();

                    let id: i64 = uri.to_string().split('=').last().unwrap().parse().unwrap();

                    tracing::debug!("got video update for id {}: {:?}", id, update);

                    let conn = handler_clone.conn.check_out().await.unwrap();
                    let video = crate::models::Video::lookup_id(&conn, id)
                        .await
                        .unwrap()
                        .unwrap();
                    crate::models::Video::set_processed_url(&conn, id, &url)
                        .await
                        .unwrap();

                    let video_return_button = handler_clone
                        .get_fluent_bundle(None, |bundle| {
                            crate::utils::get_message(&bundle, "video-return-button", None).unwrap()
                        })
                        .await;
                    let send_video = SendVideo {
                        chat_id: video.chat_id.unwrap().into(),
                        video: FileType::URL(url.to_owned()),
                        reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(
                            InlineKeyboardMarkup {
                                inline_keyboard: vec![vec![InlineKeyboardButton {
                                    text: video_return_button,
                                    switch_inline_query: Some(video.source.to_owned()),
                                    ..Default::default()
                                }]],
                            },
                        )),
                        supports_streaming: Some(true),
                        ..Default::default()
                    };
                    handler_clone.make_request(&send_video).await.unwrap();

                    count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    tracing::debug!("finished handling video");
                }
                .in_current_span(),
            );

            Ok(Response::new(Body::from("OK")))
        }
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

/// Start a web server to handle webhooks and pass updates to [handle_request].
async fn receive_webhook(host: String, secret: String, handler: Arc<MessageHandler>) {
    let addr = host.parse().expect("Invalid HTTP_HOST");

    let secret_path = format!("/{}", secret);
    let secret_path: &'static str = Box::leak(secret_path.into_boxed_str());

    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let count_clone = count.clone();
    let make_svc = hyper::service::make_service_fn(move |_conn| {
        let handler = handler.clone();
        let count = count_clone.clone();
        async move {
            Ok::<_, hyper::Error>(hyper::service::service_fn(move |req| {
                handle_request(req, count.clone(), handler.clone(), secret_path)
            }))
        }
    });

    let server = hyper::Server::bind(&addr).serve(make_svc);

    tracing::info!("listening on http://{}", addr);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal;

        let mut stream = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("unable to create terminate signal stream");

        tokio::select! {
            _ = stream.recv() => shutdown_tx.send(true),
            _ = signal::ctrl_c() => shutdown_tx.send(true),
        }
    });

    #[cfg(not(unix))]
    tokio::spawn(async move {
        use tokio::signal;

        signal::ctrl_c().await.expect("unable to await ctrl-c");
        shutdown_tx.send(true).expect("unable to send shutdown");
    });

    let graceful = server.with_graceful_shutdown(async {
        shutdown_rx.await.ok();
        tracing::error!("shutting down HTTP server");
    });

    if let Err(e) = graceful.await {
        tracing::error!("server error: {:?}", e);
    }

    loop {
        let count = count.load(std::sync::atomic::Ordering::SeqCst);
        if count == 0 {
            break;
        }

        tracing::warn!("waiting for {} requests to complete before shutdown", count);
        tokio::time::delay_for(std::time::Duration::from_millis(200)).await;
    }
}

/// Start polling updates using Bot API long polling.
async fn poll_updates(bot: Arc<Telegram>, handler: Arc<MessageHandler>) {
    let mut update_req = GetUpdates::default();
    update_req.timeout = Some(30);

    let (mut shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal;

        let mut stream = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("unable to create terminate signal stream");

        tokio::select! {
            _ = stream.recv() => shutdown_tx.send(true).await.expect("unable to send shutdown"),
            _ = signal::ctrl_c() => shutdown_tx.send(true).await.expect("unable to send shutdown"),
        };
    });

    #[cfg(not(unix))]
    tokio::spawn(async move {
        use tokio::signal;

        signal::ctrl_c().await.expect("unable to await ctrl-c");
        shutdown_tx
            .send(true)
            .await
            .expect("unable to send shutdown");
    });

    let waiting = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    loop {
        let updates = tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::error!("got shutdown request");
                loop {
                    let count = waiting.load(std::sync::atomic::Ordering::SeqCst);
                    if count == 0 {
                        break;
                    }

                    tracing::warn!("finishing {} requests before shutdown", count);
                    tokio::time::delay_for(std::time::Duration::from_millis(200)).await;
                }
                break;
            }
            updates = bot.make_request(&update_req) => updates,
        };

        let updates = match updates {
            Ok(updates) => updates,
            Err(e) => {
                sentry::capture_error(&e);
                tracing::error!("unable to get updates: {:?}", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        for update in updates {
            let id = update.update_id;

            let handler = handler.clone();
            let waiting = waiting.clone();

            tokio::spawn(async move {
                waiting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                handler.handle_update(update).await;
                tracing::debug!("finished handling update");
                waiting.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });

            update_req.offset = Some(id + 1);
        }
    }
}

// MARK: Handling updates

pub struct MessageHandler {
    // State
    pub bot_user: User,
    langs: HashMap<LanguageIdentifier, Vec<String>>,
    best_lang: RwLock<HashMap<String, fluent::concurrent::FluentBundle<fluent::FluentResource>>>,
    handlers: Vec<BoxedHandler>,

    // API clients
    pub bot: Arc<Telegram>,
    pub fapi: Arc<fuzzysearch::FuzzySearch>,
    pub influx: Arc<influxdb::Client>,
    pub finder: linkify::LinkFinder,
    pub s3: rusoto_s3::S3Client,
    pub coconut: coconut::Coconut,

    // Configuration
    pub sites: Mutex<Vec<BoxedSite>>, // We always need mutable access, no reason to use a RwLock
    pub config: Config,

    // Storage
    pub conn: quaint::pooled::Quaint,
}

impl MessageHandler {
    async fn get_fluent_bundle<C, R>(&self, requested: Option<&str>, callback: C) -> R
    where
        C: FnOnce(&fluent::concurrent::FluentBundle<fluent::FluentResource>) -> R,
    {
        let requested = if let Some(requested) = requested {
            requested
        } else {
            "en-US"
        };

        tracing::trace!(lang = requested, "looking up language bundle");

        {
            let lock = self.best_lang.read().await;
            if lock.contains_key(requested) {
                let bundle = lock.get(requested).expect("should have contained");
                return callback(bundle);
            }
        }

        tracing::info!(lang = requested, "got new language, building bundle");

        let requested_locale = requested
            .parse::<LanguageIdentifier>()
            .expect("requested locale is invalid");
        let requested_locales: Vec<LanguageIdentifier> = vec![requested_locale];
        let default_locale = L10N_LANGS[0]
            .parse::<LanguageIdentifier>()
            .expect("unable to parse langid");
        let available: Vec<LanguageIdentifier> = self.langs.keys().map(Clone::clone).collect();
        let resolved_locales = fluent_langneg::negotiate_languages(
            &requested_locales,
            &available,
            Some(&&default_locale),
            fluent_langneg::NegotiationStrategy::Filtering,
        );

        let current_locale = resolved_locales.get(0).expect("no locales were available");

        let mut bundle = fluent::concurrent::FluentBundle::<fluent::FluentResource>::new(
            resolved_locales.clone(),
        );
        let resources = self
            .langs
            .get(current_locale)
            .expect("missing known locale");

        for resource in resources {
            let resource = fluent::FluentResource::try_new(resource.to_string())
                .expect("unable to parse FTL string");
            bundle
                .add_resource(resource)
                .expect("unable to add resource");
        }

        bundle.set_use_isolating(false);

        {
            let mut lock = self.best_lang.write().await;
            lock.insert(requested.to_string(), bundle);
        }

        let lock = self.best_lang.read().await;
        let bundle = lock.get(requested).expect("value just inserted is missing");
        callback(bundle)
    }

    pub async fn report_error<C>(
        &self,
        message: &Message,
        tags: Option<Vec<(&str, String)>>,
        callback: C,
    ) where
        C: FnOnce() -> uuid::Uuid,
    {
        let u = utils::with_user_scope(message.from.as_ref(), tags, callback);

        let lang_code = message
            .from
            .as_ref()
            .map(|from| from.language_code.clone())
            .flatten();

        let msg = self
            .get_fluent_bundle(lang_code.as_deref(), |bundle| {
                if u.is_nil() {
                    utils::get_message(&bundle, "error-generic", None)
                } else {
                    let f = format!("`{}`", u.to_string());

                    let mut args = fluent::FluentArgs::new();
                    args.insert("uuid", fluent::FluentValue::from(f));

                    utils::get_message(&bundle, "error-uuid", Some(args))
                }
            })
            .await;

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: msg.unwrap(),
            parse_mode: Some(ParseMode::Markdown),
            reply_to_message_id: Some(message.message_id),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text: "Delete".to_string(),
                    callback_data: Some("delete".to_string()),
                    ..Default::default()
                }]],
            })),
            ..Default::default()
        };

        if let Err(e) = self.make_request(&send_message).await {
            tracing::error!("unable to send error message to user: {:?}", e);
            utils::with_user_scope(message.from.as_ref(), None, || {
                sentry::capture_error(&e);
            });
        }
    }

    #[tracing::instrument(skip(self, message))]
    async fn handle_welcome(&self, message: &Message, command: &str) -> anyhow::Result<()> {
        use rand::seq::SliceRandom;

        let from = message.from.as_ref().unwrap();

        let random_artwork = *STARTING_ARTWORK.choose(&mut rand::thread_rng()).unwrap();

        let try_me = self
            .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                utils::get_message(&bundle, "welcome-try-me", None).unwrap()
            })
            .await;

        let reply_markup = ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton {
                text: try_me,
                switch_inline_query_current_chat: Some(random_artwork.to_string()),
                ..Default::default()
            }]],
        });

        let name = if command == "group-add" {
            "welcome-group"
        } else {
            "welcome"
        };

        let welcome = self
            .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
                utils::get_message(&bundle, &name, None).unwrap()
            })
            .await;

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: welcome,
            reply_markup: Some(reply_markup),
            ..Default::default()
        };

        self.make_request(&send_message)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    #[tracing::instrument(skip(self, message))]
    async fn send_generic_reply(&self, message: &Message, name: &str) -> anyhow::Result<Message> {
        let language_code = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.as_deref());

        let text = self
            .get_fluent_bundle(language_code, |bundle| {
                utils::get_message(&bundle, name, None).unwrap()
            })
            .await;

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            text,
            ..Default::default()
        };

        self.make_request(&send_message).await.map_err(Into::into)
    }

    #[tracing::instrument(skip(self, update))]
    async fn handle_update(&self, update: Update) {
        sentry::configure_scope(|mut scope| {
            utils::add_sentry_tracing(&mut scope);
        });

        let user = utils::user_from_update(&update);

        if let Some(user) = user {
            tracing::trace!(user_id = user.id, "found user associated with update");
        }

        let command = update
            .message
            .as_ref()
            .and_then(|message| message.get_command());

        for handler in &self.handlers {
            tracing::trace!(handler = handler.name(), "running handler");

            match handler.handle(&self, &update, command.as_ref()).await {
                Ok(status) if status == handlers::Status::Completed => {
                    tracing::debug!(handler = handler.name(), "handled update");
                    break;
                }
                Err(e) => {
                    tracing::error!("error handling update: {:?}", e);

                    let mut tags = vec![("handler", handler.name().to_string())];
                    if let Some(user) = user {
                        tags.push(("user_id", user.id.to_string()));
                    }
                    if let Some(command) = command {
                        tags.push(("command", command.name));
                    }

                    if let Some(msg) = &update.message {
                        self.report_error(&msg, Some(tags), || capture_anyhow(&e))
                            .await;
                    } else {
                        capture_anyhow(&e);
                    }

                    break;
                }
                _ => (),
            }
        }
    }

    pub async fn make_request<T>(&self, request: &T) -> Result<T::Response, Error>
    where
        T: TelegramRequest,
    {
        use std::time::Duration;

        let mut attempts = 0;

        loop {
            let err = match self.bot.make_request(request).await {
                Ok(resp) => return Ok(resp),
                Err(err) => err,
            };

            if attempts > 2 {
                return Err(err);
            }

            let telegram_error = match err {
                tgbotapi::Error::Telegram(ref err) => err,
                _ => return Err(err),
            };

            let params = match &telegram_error.parameters {
                Some(params) => params,
                _ => return Err(err),
            };

            let retry_after = match params.retry_after {
                Some(retry_after) => retry_after,
                _ => return Err(err),
            };

            tracing::warn!(retry_after, "rate limited");

            tokio::time::delay_for(Duration::from_secs(retry_after as u64)).await;

            attempts += 1;
        }
    }
}
