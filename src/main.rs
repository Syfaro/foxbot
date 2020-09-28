#![feature(try_trait)]

use sentry::integrations::anyhow::capture_anyhow;
use sites::{PostInfo, Site};
use std::collections::HashMap;
use std::sync::Arc;
use tgbotapi::{requests::*, *};
use tokio::sync::{Mutex, RwLock};
use unic_langid::LanguageIdentifier;

mod handlers;
pub mod models;
mod sites;
mod utils;
mod video;

// MARK: Monitoring configuration

lazy_static::lazy_static! {
    static ref REQUEST_DURATION: prometheus::Histogram = prometheus::register_histogram!("foxbot_request_duration_seconds", "Time to start processing request").unwrap();
    static ref HANDLING_DURATION: prometheus::Histogram = prometheus::register_histogram!("foxbot_handling_duration_seconds", "Request processing time duration").unwrap();
    static ref ACTIVE_HANDLERS: prometheus::Gauge = prometheus::register_gauge!("foxbot_handlers_total", "Number of active handlers").unwrap();
    static ref HANDLER_DURATION: prometheus::HistogramVec = prometheus::register_histogram_vec!("foxbot_handler_duration_seconds", "Time for a handler to complete", &["handler"]).unwrap();
    static ref TELEGRAM_ERROR: prometheus::Counter = prometheus::register_counter!("foxbot_telegram_error_total", "Number of errors returned by Telegram").unwrap();
}

// MARK: Statics and types

type BoxedSite = Box<dyn Site + Send + Sync>;
type BoxedHandler = Box<dyn handlers::Handler + Send + Sync>;

static CONCURRENT_HANDLERS: usize = 2;

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

    // Logging
    jaeger_collector: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_organization_slug: Option<String>,
    pub sentry_project_slug: Option<String>,

    // Telegram config
    telegram_apitoken: String,
    pub use_webhooks: Option<bool>,
    pub webhook_endpoint: Option<String>,
    pub http_host: String,
    http_secret: Option<String>,

    // Video handling
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_token: String,
    pub s3_secret: String,
    pub s3_bucket: String,
    pub s3_url: String,

    pub fautil_apitoken: String,

    pub size_images: Option<bool>,
    pub cache_images: Option<bool>,

    redis_dsn: String,

    // Postgres database
    db_host: String,
    db_user: String,
    db_pass: String,
    db_name: String,
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

fn setup_shutdown() -> tokio::sync::mpsc::Receiver<bool> {
    let (mut shutdown_tx, shutdown_rx) = tokio::sync::mpsc::channel(1);

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal;

        let mut stream = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("unable to create terminate signal stream");

        tokio::select! {
            _ = stream.recv() => shutdown_tx.send(true).expect("unable to send shutdown"),
            _ = signal::ctrl_c() => shutdown_tx.send(true).expect("unable to send shutdown"),
        }
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

    shutdown_rx
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

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://{}:{}@{}/{}",
            config.db_user, config.db_pass, config.db_host, config.db_name
        ))
        .await
        .expect("unable to create database pool");

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

    use redis::IntoConnectionInfo;
    let redis = redis::aio::ConnectionManager::new(
        config
            .redis_dsn
            .clone()
            .into_connection_info()
            .expect("Unable to parse Redis DSN"),
    )
    .await
    .expect("Unable to open Redis connection");

    let handler = Arc::new(MessageHandler {
        bot_user,
        langs,
        best_lang: RwLock::new(HashMap::new()),
        handlers,
        config: config.clone(),

        bot: bot.clone(),
        fapi,
        finder,
        s3,

        sites: Mutex::new(sites),
        conn: pool,
        redis,
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

    // Allow buffering more updates than can be run at once
    let (tx, rx) = tokio::sync::mpsc::channel(CONCURRENT_HANDLERS * 2);
    let shutdown = setup_shutdown();

    let use_webhooks = matches!(config.use_webhooks, Some(use_webhooks) if use_webhooks);

    if use_webhooks {
        let webhook_endpoint = config.webhook_endpoint.expect("Missing WEBHOOK_ENDPOINT");
        let set_webhook = SetWebhook {
            url: webhook_endpoint,
        };
        if let Err(e) = bot.make_request(&set_webhook).await {
            panic!(e);
        }

        receive_webhook(
            tx,
            shutdown,
            config.http_host,
            config.http_secret.expect("Missing HTTP_SECRET"),
        )
        .await;
    } else {
        let delete_webhook = DeleteWebhook;
        if let Err(e) = bot.make_request(&delete_webhook).await {
            panic!(e);
        }

        poll_updates(tx, shutdown, bot).await;
    }

    use futures::StreamExt;
    rx.for_each_concurrent(CONCURRENT_HANDLERS, |update| {
        let handler = handler.clone();
        async move {
            let update = *update;

            tracing::trace!(?update, "handling update");
            handler.handle_update(update).await;
        }
    })
    .await;
}

// MARK: Get Bot API updates

/// Handle an incoming HTTP POST request to /{token}.
///
/// It spawns a handler for each request.
#[tracing::instrument(skip(req, tx, secret))]
async fn handle_request(
    req: hyper::Request<hyper::Body>,
    mut tx: tokio::sync::mpsc::Sender<Box<tgbotapi::Update>>,
    secret: &str,
) -> hyper::Result<hyper::Response<hyper::Body>> {
    use hyper::{Body, Response, StatusCode};

    tracing::trace!(method = ?req.method(), path = req.uri().path(), "got HTTP request");

    match (req.method(), req.uri().path()) {
        (&hyper::Method::GET, "/health") => Ok(Response::new(Body::from("OK"))),
        (&hyper::Method::GET, "/metrics") => {
            tracing::trace!("encoding metrics");

            use prometheus::Encoder;
            let encoder = prometheus::TextEncoder::new();
            let metric_families = prometheus::gather();
            let mut buf = vec![];
            encoder.encode(&metric_families, &mut buf).unwrap();

            Ok(Response::new(Body::from(buf)))
        }
        (&hyper::Method::POST, path) if path == secret => {
            let _hist = REQUEST_DURATION.start_timer();

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

            tx.send(Box::new(update)).await.unwrap();

            Ok(Response::new(Body::from("✓")))
        }
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

/// Start a web server to handle webhooks and pass updates to [handle_request].
async fn receive_webhook(
    tx: tokio::sync::mpsc::Sender<Box<tgbotapi::Update>>,
    mut shutdown: tokio::sync::mpsc::Receiver<bool>,
    host: String,
    secret: String,
) {
    let addr = host.parse().expect("Invalid HTTP_HOST");

    let secret_path = format!("/{}", secret);
    let secret_path: &'static str = Box::leak(secret_path.into_boxed_str());

    let make_svc = hyper::service::make_service_fn(move |_conn| {
        let tx = tx.clone();
        async move {
            Ok::<_, hyper::Error>(hyper::service::service_fn(move |req| {
                handle_request(req, tx.clone(), secret_path)
            }))
        }
    });

    tokio::spawn(async move {
        tracing::info!("listening on http://{}", addr);

        let graceful = hyper::Server::bind(&addr)
            .serve(make_svc)
            .with_graceful_shutdown(async {
                shutdown.recv().await;
                tracing::error!("shutting down http server");
            });

        if let Err(e) = graceful.await {
            tracing::error!("server error: {:?}", e);
        }
    });
}

/// Start polling updates using Bot API long polling.
async fn poll_updates(
    mut tx: tokio::sync::mpsc::Sender<Box<tgbotapi::Update>>,
    mut shutdown: tokio::sync::mpsc::Receiver<bool>,
    bot: Arc<Telegram>,
) {
    let mut update_req = GetUpdates::default();
    update_req.timeout = Some(30);

    tokio::spawn(async move {
        loop {
            let updates = tokio::select! {
                _ = shutdown.recv() => {
                    tracing::error!("got shutdown request");
                    break;
                }

                updates = bot.make_request(&update_req) => updates,
            };

            let updates = match updates {
                Ok(updates) => updates,
                Err(e) => {
                    sentry::capture_error(&e);
                    tracing::error!("unable to get updates: {:?}", e);
                    tokio::time::delay_for(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            for update in updates {
                let id = update.update_id;

                tx.send(Box::new(update)).await.unwrap();

                update_req.offset = Some(id + 1);
            }
        }
    });
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
    pub finder: linkify::LinkFinder,
    pub s3: rusoto_s3::S3Client,

    // Configuration
    pub sites: Mutex<Vec<BoxedSite>>, // We always need mutable access, no reason to use a RwLock
    pub config: Config,

    // Storage
    pub conn: sqlx::Pool<sqlx::Postgres>,
    pub redis: redis::aio::ConnectionManager,
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

        use redis::AsyncCommands;
        let mut conn = self.redis.clone();
        let key_list = format!("errors:{}", message.chat.id);
        let key_message_id = format!("errors:message-id:{}", message.chat.id);
        let recent_error_count: i32 = conn.llen(&key_list).await.ok().unwrap_or(0);

        if let Err(e) = conn.lpush::<_, _, ()>(&key_list, u.to_string()).await {
            tracing::error!("unable to insert error uuid in redis: {:?}", e);
            sentry::capture_error(&e);
        };

        if let Err(e) = conn.expire::<_, ()>(&key_list, 60 * 5).await {
            tracing::error!("unable to set redis error expire: {:?}", e);
            sentry::capture_error(&e);
        }

        if let Err(e) = conn.expire::<_, ()>(&key_message_id, 60 * 5).await {
            tracing::error!("unable to set redis error message id expire: {:?}", e);
            sentry::capture_error(&e);
        }

        let msg = self
            .get_fluent_bundle(lang_code.as_deref(), |bundle| {
                let mut args = fluent::FluentArgs::new();
                args.insert("count", (recent_error_count + 1).into());

                if u.is_nil() {
                    if recent_error_count > 0 {
                        utils::get_message(&bundle, "error-generic-count", Some(args))
                    } else {
                        utils::get_message(&bundle, "error-generic", None)
                    }
                } else {
                    let f = format!("`{}`", u.to_string());
                    args.insert("uuid", fluent::FluentValue::from(f));

                    let name = if recent_error_count > 0 {
                        "error-uuid-count"
                    } else {
                        "error-uuid"
                    };

                    utils::get_message(&bundle, name, Some(args))
                }
            })
            .await
            .unwrap();

        let delete_markup = Some(ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton {
                text: "Delete".to_string(),
                callback_data: Some("delete".to_string()),
                ..Default::default()
            }]],
        }));

        if recent_error_count > 0 {
            let message_id: i32 = match conn.get(&key_message_id).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("unable to get error message-id to edit: {:?}", e);
                    utils::with_user_scope(message.from.as_ref(), None, || {
                        sentry::capture_error(&e);
                    });

                    return;
                }
            };

            let edit_message = EditMessageText {
                chat_id: message.chat_id(),
                message_id: Some(message_id),
                text: msg,
                parse_mode: Some(ParseMode::Markdown),
                reply_markup: delete_markup,
                ..Default::default()
            };

            if let Err(e) = self.make_request(&edit_message).await {
                tracing::error!("unable to edit error message to user: {:?}", e);
                utils::with_user_scope(message.from.as_ref(), None, || {
                    sentry::capture_error(&e);
                });

                let _ = conn.del::<_, ()>(&key_list).await;
                let _ = conn.del::<_, ()>(&key_message_id).await;
            }
        } else {
            let send_message = SendMessage {
                chat_id: message.chat_id(),
                text: msg,
                parse_mode: Some(ParseMode::Markdown),
                reply_to_message_id: Some(message.message_id),
                reply_markup: delete_markup,
                ..Default::default()
            };

            match self.make_request(&send_message).await {
                Ok(resp) => {
                    if let Err(e) = conn
                        .set_ex::<_, _, ()>(key_message_id, resp.message_id, 60 * 5)
                        .await
                    {
                        tracing::error!("unable to set redis error message id: {:?}", e);
                        sentry::capture_error(&e);
                    }
                }
                Err(e) => {
                    tracing::error!("unable to send error message to user: {:?}", e);
                    utils::with_user_scope(message.from.as_ref(), None, || {
                        sentry::capture_error(&e);
                    });
                }
            }
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
        let _hist = HANDLING_DURATION.start_timer();

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
            let hist = HANDLER_DURATION
                .get_metric_with_label_values(&[handler.name()])
                .unwrap()
                .start_timer();

            match handler.handle(&self, &update, command.as_ref()).await {
                Ok(status) if status == handlers::Status::Completed => {
                    tracing::debug!(handler = handler.name(), "handled update");
                    hist.stop_and_record();
                    break;
                }
                Err(e) => {
                    tracing::error!("error handling update: {:?}", e);
                    hist.stop_and_record();

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
                _ => {
                    hist.stop_and_discard();
                }
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

            TELEGRAM_ERROR.inc();

            if attempts > 2 {
                return Err(err);
            }

            let retry_after = match err {
                tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                    parameters:
                        Some(tgbotapi::ResponseParameters {
                            retry_after: Some(retry_after),
                            ..
                        }),
                    ..
                }) => retry_after,
                _ => return Err(err),
            };

            tracing::warn!(retry_after, "rate limited");

            tokio::time::delay_for(Duration::from_secs(retry_after as u64)).await;

            attempts += 1;
        }
    }
}
