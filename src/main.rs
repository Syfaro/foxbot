#![feature(try_trait)]

use sentry::integrations::failure::{capture_error, capture_fail};
use sites::{PostInfo, Site};
use std::collections::HashMap;
use std::sync::Arc;
use tgbotapi::{requests::*, *};
use tokio::sync::{Mutex, RwLock};
use tracing_futures::Instrument;
use unic_langid::LanguageIdentifier;

#[macro_use]
extern crate failure;

mod handlers;
mod migrations;
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

    // Others
    pub fautil_apitoken: String,
    pub use_proxy: Option<bool>,
    pub database: String,
}

// MARK: Initialization

/// Configure tracing with Jaeger.
fn configure_tracing(collector: String) {
    use opentelemetry::{
        api::{KeyValue, Provider, Sampler},
        exporter::trace::jaeger,
        sdk::Config as TelemConfig,
    };
    use std::net::ToSocketAddrs;
    use tracing_subscriber::layer::SubscriberExt;

    let env = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let addr = collector
        .to_socket_addrs()
        .expect("Unable to resolve JAEGER_COLLECTOR")
        .next()
        .expect("Unable to find JAEGER_COLLECTOR");

    let exporter = jaeger::Exporter::builder()
        .with_collector_endpoint(addr)
        .with_process(jaeger::Process {
            service_name: "foxbot",
            tags: vec![
                KeyValue::new("environment", env),
                KeyValue::new("version", env!("CARGO_PKG_VERSION")),
            ],
        })
        .init();

    let provider = opentelemetry::sdk::Provider::builder()
        .with_exporter(exporter)
        .with_config(TelemConfig {
            default_sampler: Sampler::Always,
            ..Default::default()
        })
        .build();

    opentelemetry::global::set_provider(provider);

    let tracer = opentelemetry::global::trace_provider().get_tracer("telegram");

    let telem_layer = tracing_opentelemetry::OpentelemetryLayer::with_tracer(tracer);
    let fmt_layer = tracing_subscriber::fmt::Layer::default();

    let subscriber = tracing_subscriber::Registry::default()
        .with(telem_layer)
        .with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set default tracing subscriber");
}

async fn run_migrations(database: &str) {
    let mut conn = rusqlite::Connection::open(database).expect("Unable to open database");

    migrations::runner()
        .run(&mut conn)
        .expect("Unable to migrate database");
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let jaeger_collector = match &config.jaeger_collector {
        Some(collector) => collector.to_owned(),
        _ => panic!("Missing JAEGER_COLLECTOR"),
    };

    configure_tracing(jaeger_collector);

    run_migrations(&config.database).await;

    let pool = quaint::pooled::Quaint::new(&format!("file:{}", config.database))
        .await
        .expect("Unable to connect to database");

    let fapi = Arc::new(fautil::FAUtil::new(config.fautil_apitoken.clone()));

    let sites: Vec<BoxedSite> = vec![
        Box::new(sites::E621::new()),
        Box::new(sites::FurAffinity::new(
            (config.fa_a.clone(), config.fa_b.clone()),
            config.fautil_apitoken.clone(),
        )),
        Box::new(sites::Weasyl::new(config.weasyl_apitoken.clone())),
        Box::new(sites::Twitter::new(
            config.twitter_consumer_key.clone(),
            config.twitter_consumer_secret.clone(),
            pool.clone(),
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

    use std::io::Read;

    let mut langs = HashMap::new();

    for lang in L10N_LANGS {
        let path = dir.join(lang);

        let mut lang_resources = Vec::with_capacity(L10N_RESOURCES.len());
        let langid = lang
            .parse::<LanguageIdentifier>()
            .expect("Unable to parse language");

        for resource in L10N_RESOURCES {
            let file = path.join(resource);
            let mut f = std::fs::File::open(file).expect("Unable to open language");
            let mut s = String::new();
            f.read_to_string(&mut s).expect("Unable to read file");

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
    ];

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

        sites: Mutex::new(sites),
        conn: pool,
    });

    let _guard = if let Some(dsn) = config.sentry_dsn {
        sentry::integrations::panic::register_panic_handler();
        Some(sentry::init(sentry::ClientOptions {
            dsn: Some(dsn.parse().unwrap()),
            release: option_env!("RELEASE").map(std::borrow::Cow::from),
            ..Default::default()
        }))
    } else {
        None
    };

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
    handler: Arc<MessageHandler>,
    secret: &str,
) -> hyper::Result<hyper::Response<hyper::Body>> {
    use hyper::{Body, Response, StatusCode};

    let now = std::time::Instant::now();

    tracing::trace!("got HTTP request: {} {}", req.method(), req.uri().path());

    match (req.method(), req.uri().path()) {
        (&hyper::Method::POST, path) if path == secret => {
            tracing::trace!("handling update");
            let body = req.into_body();
            let bytes = hyper::body::to_bytes(body)
                .await
                .expect("Unable to read body bytes");
            let update: Update = match serde_json::from_slice(&bytes) {
                Ok(update) => update,
                Err(_) => {
                    let mut resp = Response::default();
                    *resp.status_mut() = StatusCode::BAD_REQUEST;
                    return Ok(resp);
                }
            };

            let handler_clone = handler.clone();
            tokio::spawn(
                async move {
                    tracing::debug!("got update: {:?}", update);
                    handler_clone.handle_update(update).await;
                    tracing::event!(tracing::Level::DEBUG, "finished handling message");
                }
                .in_current_span(),
            );

            let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "http")
                .add_field("duration", now.elapsed().as_millis() as i64);

            if let Err(e) = handler.influx.query(&point).await {
                capture_fail(&e);
                tracing::error!("unable to send http request info to InfluxDB: {:?}", e);
            }

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
async fn receive_webhook(host: String, secret: String, handler: Arc<MessageHandler>) {
    let addr = host.parse().expect("Invalid HTTP_HOST");

    let secret_path = format!("/{}", secret);
    let secret_path: &'static str = Box::leak(secret_path.into_boxed_str());

    let make_svc = hyper::service::make_service_fn(move |_conn| {
        let handler = handler.clone();
        async move {
            Ok::<_, hyper::Error>(hyper::service::service_fn(move |req| {
                handle_request(req, handler.clone(), secret_path)
            }))
        }
    });

    let server = hyper::Server::bind(&addr).serve(make_svc);

    tracing::info!("listening on http://{}", addr);

    if let Err(e) = server.await {
        tracing::error!("server error: {:?}", e);
    }
}

/// Start polling updates using Bot API long polling.
async fn poll_updates(bot: Arc<Telegram>, handler: Arc<MessageHandler>) {
    let mut update_req = GetUpdates::default();
    update_req.timeout = Some(30);

    loop {
        let updates = match bot.make_request(&update_req).await {
            Ok(updates) => updates,
            Err(e) => {
                capture_fail(&e);
                tracing::error!("unable to get updates: {:?}", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        for update in updates {
            let id = update.update_id;

            let handler = handler.clone();

            tokio::spawn(async move {
                handler.handle_update(update).await;
                tracing::event!(tracing::Level::DEBUG, "finished handling update");
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
    best_lang: RwLock<HashMap<String, fluent::FluentBundle<fluent::FluentResource>>>,
    handlers: Vec<BoxedHandler>,

    // API clients
    pub bot: Arc<Telegram>,
    pub fapi: Arc<fautil::FAUtil>,
    pub influx: Arc<influxdb::Client>,
    pub finder: linkify::LinkFinder,

    // Configuration
    pub sites: Mutex<Vec<BoxedSite>>, // We always need mutable access, no reason to use a RwLock
    pub config: Config,

    // Storage
    pub conn: quaint::pooled::Quaint,
}

impl MessageHandler {
    async fn get_fluent_bundle<C, R>(&self, requested: Option<&str>, callback: C) -> R
    where
        C: FnOnce(&fluent::FluentBundle<fluent::FluentResource>) -> R,
    {
        let requested = if let Some(requested) = requested {
            requested
        } else {
            "en-US"
        };

        tracing::trace!("looking up language bundle for {}", requested);

        {
            let lock = self.best_lang.read().await;
            if lock.contains_key(requested) {
                let bundle = lock.get(requested).expect("Should have contained");
                return callback(bundle);
            }
        }

        tracing::info!("got new language {}, building bundle", requested);

        let requested_locale = requested
            .parse::<LanguageIdentifier>()
            .expect("Requested locale is invalid");
        let requested_locales: Vec<LanguageIdentifier> = vec![requested_locale];
        let default_locale = L10N_LANGS[0]
            .parse::<LanguageIdentifier>()
            .expect("Unable to parse langid");
        let available: Vec<LanguageIdentifier> = self.langs.keys().map(Clone::clone).collect();
        let resolved_locales = fluent_langneg::negotiate_languages(
            &requested_locales,
            &available,
            Some(&&default_locale),
            fluent_langneg::NegotiationStrategy::Filtering,
        );

        let current_locale = resolved_locales.get(0).expect("No locales were available");

        let mut bundle =
            fluent::FluentBundle::<fluent::FluentResource>::new(resolved_locales.clone());
        let resources = self
            .langs
            .get(current_locale)
            .expect("Missing known locale");

        for resource in resources {
            let resource = fluent::FluentResource::try_new(resource.to_string())
                .expect("Unable to parse FTL string");
            bundle
                .add_resource(resource)
                .expect("Unable to add resource");
        }

        bundle.set_use_isolating(false);

        {
            let mut lock = self.best_lang.write().await;
            lock.insert(requested.to_string(), bundle);
        }

        let lock = self.best_lang.read().await;
        let bundle = lock.get(requested).expect("Value just inserted is missing");
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
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&send_message).await {
            tracing::error!("unable to send error message to user: {:?}", e);
            utils::with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }

    #[tracing::instrument(skip(self, message))]
    async fn handle_welcome(&self, message: &Message, command: &str) -> failure::Fallible<()> {
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

        self.bot
            .make_request(&send_message)
            .await
            .map(|_msg| ())
            .map_err(Into::into)
    }

    #[tracing::instrument(skip(self, message))]
    async fn send_generic_reply(
        &self,
        message: &Message,
        name: &str,
    ) -> failure::Fallible<Message> {
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

        self.bot
            .make_request(&send_message)
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip(self, update))]
    async fn handle_update(&self, update: Update) {
        let user = update
            .message
            .as_ref()
            .and_then(|message| message.from.as_ref());

        for handler in &self.handlers {
            let command = update
                .message
                .as_ref()
                .and_then(|message| message.get_command());

            tracing::trace!(handler = handler.name(), "running handler");

            match handler.handle(&self, &update, command.as_ref()).await {
                Ok(status) if status == handlers::Status::Completed => break,
                Err(e) => {
                    log::error!("Error handling update: {:#?}", e);

                    let mut tags = vec![("handler", handler.name().to_string())];
                    if let Some(user) = user {
                        tags.push(("user_id", user.id.to_string()));
                    }
                    if let Some(command) = command {
                        tags.push(("command", command.name));
                    }

                    if let Some(msg) = &update.message {
                        self.report_error(&msg, Some(tags), || capture_error(&e))
                            .await;
                    } else {
                        capture_error(&e);
                    }

                    break;
                }
                _ => (),
            }
        }
    }
}
