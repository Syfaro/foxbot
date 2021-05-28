use sentry::integrations::anyhow::capture_anyhow;
use std::collections::HashMap;
use std::sync::Arc;
use tgbotapi::{requests::*, *};
use tokio::sync::{Mutex, RwLock};
use tracing::Instrument;
use unic_langid::LanguageIdentifier;

use foxbot_models::DisplayableErrorMessage;
use foxbot_utils::*;

mod coconut;
mod handlers;
mod web;

lazy_static::lazy_static! {
    static ref REQUEST_DURATION: prometheus::Histogram = prometheus::register_histogram!("foxbot_request_duration_seconds", "Time to start processing request").unwrap();
    static ref HANDLING_DURATION: prometheus::Histogram = prometheus::register_histogram!("foxbot_handling_duration_seconds", "Request processing time duration").unwrap();
    static ref HANDLER_DURATION: prometheus::HistogramVec = prometheus::register_histogram_vec!("foxbot_handler_duration_seconds", "Time for a handler to complete", &["handler"]).unwrap();
    static ref TELEGRAM_REQUEST: prometheus::Counter = prometheus::register_counter!("foxbot_telegram_request_total", "Number of requests made to Telegram").unwrap();
    static ref TELEGRAM_ERROR: prometheus::Counter = prometheus::register_counter!("foxbot_telegram_error_total", "Number of errors returned by Telegram").unwrap();
}

type BoxedHandler = Box<dyn handlers::Handler + Send + Sync>;
pub type UpdateSender = tokio::sync::mpsc::Sender<(HandlerUpdate, tracing::Span)>;

static CONCURRENT_HANDLERS: usize = 2;
static INLINE_HANDLERS: usize = 10;

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
    pub e621_login: String,
    pub e621_api_key: String,
    pub fautil_apitoken: String,

    // Twitter config
    pub twitter_consumer_key: String,
    pub twitter_consumer_secret: String,
    pub twitter_callback: String,

    // Logging
    jaeger_collector: Option<String>,
    pub sentry_dsn: Option<String>,
    pub sentry_organization_slug: Option<String>,
    pub sentry_project_slug: Option<String>,

    // Telegram config
    telegram_apitoken: String,

    // File storage
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_token: String,
    pub s3_secret: String,
    pub s3_bucket: String,
    pub s3_url: String,

    // Video storage
    b2_account_id: String,
    b2_app_key: String,
    b2_bucket_id: String,

    // Video encoding
    coconut_apitoken: String,
    coconut_secret: String,

    // Inline image processing options
    pub cache_all_images: Option<bool>,

    // Connections
    redis_dsn: String,
    faktory_url: Option<String>,
    database_url: String,

    // Public URLs and secrets
    internet_url: String,
    internal_secret: String,
}

/// Configure tracing with Jaeger.
fn configure_tracing(collector: String) {
    use opentelemetry::KeyValue;
    use tracing_subscriber::layer::SubscriberExt;

    tracing_log::LogTracer::init().unwrap();

    let env = std::env::var("ENVIRONMENT");
    let env = if let Ok(env) = env.as_ref() {
        env.as_str()
    } else if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());

    let tracer = opentelemetry_jaeger::new_pipeline()
        .with_agent_endpoint(collector)
        .with_service_name("foxbot")
        .with_tags(vec![
            KeyValue::new("environment", env.to_owned()),
            KeyValue::new("version", env!("CARGO_PKG_VERSION")),
        ])
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap();

    let trace = tracing_opentelemetry::layer().with_tracer(tracer);
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();

    if matches!(std::env::var("LOG_FMT").as_deref(), Ok("json")) {
        let subscriber = tracing_subscriber::fmt::layer()
            .json()
            .with_timer(tracing_subscriber::fmt::time::ChronoUtc::rfc3339())
            .with_target(true);
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter)
            .with(trace)
            .with(subscriber);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    } else {
        let subscriber = tracing_subscriber::fmt::layer();
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter)
            .with(trace)
            .with(subscriber);
        tracing::subscriber::set_global_default(subscriber).unwrap();
    }
}

#[cfg(feature = "env")]
fn load_env() {
    dotenv::dotenv().unwrap();
}

#[cfg(not(feature = "env"))]
fn load_env() {}

#[derive(Debug)]
pub enum ServiceData {
    VideoProgress {
        display_name: String,
        progress: String,
    },
    VideoComplete {
        display_name: String,
        video_url: String,
        thumb_url: String,
    },
    TwitterVerified {
        token: String,
        verifier: String,
    },
    NewHash {
        hash: i64,
    },
}

#[derive(Debug)]
pub enum HandlerUpdate {
    Telegram(Box<tgbotapi::Update>),
    Service(ServiceData),
}

impl From<Box<tgbotapi::Update>> for HandlerUpdate {
    fn from(update: Box<tgbotapi::Update>) -> Self {
        Self::Telegram(update)
    }
}

#[tokio::main]
async fn main() {
    load_env();

    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let jaeger_collector = match &config.jaeger_collector {
        Some(collector) => collector.clone(),
        _ => panic!("Missing JAEGER_COLLECTOR"),
    };

    configure_tracing(jaeger_collector);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .expect("unable to create database pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("unable to run database migrations");

    let fapi = Arc::new(fuzzysearch::FuzzySearch::new(
        config.fautil_apitoken.clone(),
    ));

    let sites = foxbot_sites::get_all_sites(
        config.fa_a.clone(),
        config.fa_b.clone(),
        config.fautil_apitoken.clone(),
        config.weasyl_apitoken.clone(),
        config.twitter_consumer_key.clone(),
        config.twitter_consumer_secret.clone(),
        config.inkbunny_username.clone(),
        config.inkbunny_password.clone(),
        config.e621_login.clone(),
        config.e621_api_key.clone(),
        pool.clone(),
    )
    .await;

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
        Box::new(handlers::ErrorReplyHandler::new()),
        Box::new(handlers::SettingsHandler),
        Box::new(handlers::TwitterHandler),
        Box::new(handlers::SubscribeHandler),
        Box::new(handlers::ErrorCleanup),
        Box::new(handlers::PermissionHandler),
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
        format!("{}/coconut/{}", config.internet_url, config.coconut_secret),
        config.b2_account_id.clone(),
        config.b2_app_key.clone(),
        config.b2_bucket_id.clone(),
    );

    let redis_client = redis::Client::open(config.redis_dsn.clone()).unwrap();
    let redis = redis::aio::ConnectionManager::new(redis_client)
        .await
        .expect("Unable to open Redis connection");

    let faktory = faktory::Producer::connect(config.faktory_url.as_deref())
        .expect("Unable to connect to Faktory");

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
        coconut,
        faktory: Arc::new(std::sync::Mutex::new(faktory)),

        sites: Mutex::new(sites),
        conn: pool.clone(),
        redis,
    });

    let _guard = config.sentry_dsn.as_ref().map(|sentry_dsn| {
        sentry::init(sentry::ClientOptions {
            dsn: Some(sentry_dsn.parse().unwrap()),
            debug: true,
            release: option_env!("RELEASE").map(std::borrow::Cow::from),
            attach_stacktrace: true,
            ..Default::default()
        })
    });

    tracing::info!(
        "sentry enabled: {}",
        _guard
            .as_ref()
            .map_or(false, sentry::ClientInitGuard::is_enabled)
    );

    // Allow buffering more updates than can be run at once
    let (update_tx, update_rx) = tokio::sync::mpsc::channel(CONCURRENT_HANDLERS * 2);
    let (inline_tx, inline_rx) = tokio::sync::mpsc::channel(INLINE_HANDLERS * 2);

    let webhook_endpoint = format!(
        "{}/telegram/{}",
        config.internet_url, config.telegram_apitoken
    );
    let set_webhook = SetWebhook {
        url: webhook_endpoint.clone(),
        allowed_updates: Some(vec![
            "message".into(),
            "channel_post".into(),
            "inline_query".into(),
            "chosen_inline_result".into(),
            "callback_query".into(),
            "my_chat_member".into(),
            "chat_member".into(),
        ]),
    };
    if let Err(e) = bot.make_request(&set_webhook).await {
        panic!("unable to set webhook: {:?}", e);
    }

    std::thread::spawn(|| {
        actix_web::rt::System::new().block_on(async move {
            web::serve(config, inline_tx, update_tx, pool, bot).await;
        });
    });

    // We have broken updates into two categories, inline queries and everything
    // else. Inline queries must be quickly answered otherwise users will assume
    // something has gone wrong or Telegram will expire the query and prevent us
    // from answering it. We can address this problem by running two separate
    // worker queues. All inline queries are run together with a much higher
    // concurrency limit. All other updates are run on a queue with lower
    // concurrency limits as they are not as time-sensitive.

    use futures::StreamExt;
    use tokio_stream::wrappers::ReceiverStream;

    // Spawn a new worker for inline queries, limited by `INLINE_HANDLERS`.
    let h = handler.clone();
    tokio::spawn(async move {
        ReceiverStream::new(inline_rx)
            .for_each_concurrent(INLINE_HANDLERS, |(inline_query, span)| {
                let handler = h.clone();

                async move {
                    tokio::spawn(async move {
                        handler.handle_update(inline_query).instrument(span).await;
                    })
                    .await
                    .unwrap();
                }
            })
            .await;
    });

    // Process all other updates, limited by `CONCURRENT_HANDLERS`.
    ReceiverStream::new(update_rx)
        .for_each_concurrent(CONCURRENT_HANDLERS, |(update, span)| {
            let handler = handler.clone();

            async move {
                tokio::spawn(async move {
                    handler.handle_update(update).instrument(span).await;
                })
                .await
                .unwrap();
            }
        })
        .await;

    opentelemetry::global::shutdown_tracer_provider();
}

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
    pub coconut: coconut::Coconut,
    pub faktory: Arc<std::sync::Mutex<faktory::Producer<std::net::TcpStream>>>,

    // Configuration
    pub sites: Mutex<Vec<foxbot_sites::BoxedSite>>, // We always need mutable access, no reason to use a RwLock
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
        let requested = requested.unwrap_or(L10N_LANGS[0]);

        tracing::trace!(lang = requested, "looking up language bundle");

        {
            let lock = self.best_lang.read().await;
            if let Some(bundle) = lock.get(requested) {
                return callback(bundle);
            }
        }

        tracing::info!(lang = requested, "got new language, building bundle");

        let bundle = get_lang_bundle(&self.langs, requested);

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
        err: anyhow::Error,
        tags: Option<Vec<(&str, String)>>,
        callback: C,
    ) where
        C: FnOnce(&anyhow::Error) -> uuid::Uuid,
    {
        let u = with_user_scope(message.from.as_ref(), &err, tags, callback);

        let lang_code = message
            .from
            .as_ref()
            .and_then(|from| from.language_code.clone());

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

        let displayable_error: Option<String> = err
            .downcast_ref::<DisplayableErrorMessage>()
            .map(|err| err.msg.clone().to_string());

        let msg = self
            .get_fluent_bundle(lang_code.as_deref(), |bundle| {
                let mut args = fluent::FluentArgs::new();
                args.insert("count", (recent_error_count + 1).into());

                let has_displayable_error = if let Some(displayable_error) = displayable_error {
                    args.insert("message", displayable_error.into());
                    true
                } else {
                    false
                };

                if u.is_nil() {
                    if recent_error_count > 0 {
                        get_message(bundle, "error-generic-count", Some(args))
                    } else if has_displayable_error {
                        get_message(bundle, "error-generic-message", Some(args))
                    } else {
                        get_message(bundle, "error-generic", None)
                    }
                } else {
                    let f = format!("`{}`", u.to_string());
                    args.insert("uuid", fluent::FluentValue::from(f));

                    let name = if recent_error_count > 0 {
                        "error-uuid-count"
                    } else if has_displayable_error {
                        "error-uuid-message"
                    } else {
                        "error-uuid"
                    };

                    get_message(bundle, name, Some(args))
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
                    with_user_scope(message.from.as_ref(), &e.into(), None, |err| {
                        sentry::integrations::anyhow::capture_anyhow(&err);
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
                with_user_scope(message.from.as_ref(), &e.into(), None, |err| {
                    sentry::integrations::anyhow::capture_anyhow(&err);
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
                    with_user_scope(message.from.as_ref(), &e.into(), None, |err| {
                        sentry::integrations::anyhow::capture_anyhow(&err);
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
                get_message(bundle, "welcome-try-me", None).unwrap()
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
                get_message(bundle, name, None).unwrap()
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
                get_message(bundle, name, None).unwrap()
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

    #[tracing::instrument(skip(self, handler_update), fields(user_id, chat_id))]
    async fn handle_update(&self, handler_update: HandlerUpdate) {
        let _hist = HANDLING_DURATION.start_timer();

        tracing::trace!(?handler_update, "handling update");

        sentry::configure_scope(|mut scope| {
            add_sentry_tracing(&mut scope);
        });

        let update = match handler_update {
            HandlerUpdate::Service(service_data) => {
                tracing::debug!("got service update: {:?}", service_data);

                for handler in &self.handlers {
                    if let Err(err) = handler.handle_service(self, &service_data).await {
                        tracing::error!("unable to handle service update: {:?}", err);
                        capture_anyhow(&err);
                    }
                }

                return;
            }
            HandlerUpdate::Telegram(update) => update,
        };

        let user = user_from_update(&update);
        let chat = chat_from_update(&update);

        if let Some(user) = user {
            tracing::Span::current().record("user_id", &user.id);
        }

        if let Some(chat) = chat {
            tracing::Span::current().record("chat_id", &chat.id);
        }

        let command = update
            .message
            .as_ref()
            .and_then(|message| message.get_command());

        for handler in &self.handlers {
            let hist = HANDLER_DURATION
                .get_metric_with_label_values(&[handler.name()])
                .unwrap()
                .start_timer();

            match handler
                .handle(self, &update, command.as_ref())
                .instrument(tracing::info_span!(
                    "handler_handle",
                    handler = handler.name()
                ))
                .await
            {
                Ok(status) if status == handlers::Status::Completed => {
                    tracing::debug!(handled_by = handler.name(), "Completed update");
                    hist.stop_and_record();

                    break;
                }
                Err(err) => {
                    tracing::error!(handled_by = handler.name(), "Handler error: {:?}", err);
                    hist.stop_and_record();

                    let mut tags = vec![("handler", handler.name().to_string())];
                    if let Some(user) = user {
                        tags.push(("user_id", user.id.to_string()));
                    }
                    if let Some(chat) = chat {
                        tags.push(("chat_id", chat.id.to_string()));
                    }
                    if let Some(command) = command {
                        tags.push(("command", command.name));
                    }

                    if let Some(msg) = &update.message {
                        self.report_error(msg, err, Some(tags), |err| capture_anyhow(&err))
                            .await;
                    } else {
                        capture_anyhow(&err);
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

        TELEGRAM_REQUEST.inc();

        let mut attempts = 0;

        loop {
            let err = match self.bot.make_request(request).await {
                Ok(resp) => return Ok(resp),
                Err(err) => err,
            };

            if attempts > 2 {
                TELEGRAM_ERROR.inc();
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
                }) => {
                    tracing::warn!(retry_after, "Rate limited");
                    retry_after
                }
                tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                    error_code: Some(400),
                    description: Some(desc),
                    ..
                }) if desc
                    == "Bad Request: wrong file_id or the file is temporarily unavailable" =>
                {
                    tracing::warn!("file_id temporarily unavailable");
                    2
                }
                tgbotapi::Error::Request(err) => {
                    tracing::warn!("Telegram request network error: {:?}", err);
                    2
                }
                _ => {
                    TELEGRAM_ERROR.inc();
                    return Err(err);
                }
            };

            tokio::time::sleep(Duration::from_secs(retry_after as u64)).await;

            attempts += 1;
        }
    }
}

#[cfg(test)]
pub mod test_helpers {
    pub async fn get_redis() -> redis::aio::ConnectionManager {
        let redis_client =
            redis::Client::open(std::env::var("REDIS_DSN").expect("Missing REDIS_DSN")).unwrap();
        redis::aio::ConnectionManager::new(redis_client)
            .await
            .expect("Unable to open Redis connection")
    }
}
