use std::ops::Add;
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, future::Future};

use opentelemetry::propagation::TextMapPropagator;
use tgbotapi::{
    requests::{EditMessageCaption, EditMessageReplyMarkup, GetMe, ReplyMarkup},
    InlineKeyboardButton, InlineKeyboardMarkup,
};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use foxbot_models::Sites;
use foxbot_sites::BoxedSite;
use foxbot_utils::*;

mod channel;
mod group;
mod subscribe;

fn main() {
    use opentelemetry::KeyValue;
    use tracing_subscriber::layer::SubscriberExt;

    load_env();
    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };
    let config_clone = config.clone();

    let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

    let env = std::env::var("ENVIRONMENT");
    let env = if let Ok(env) = env.as_ref() {
        env.as_str()
    } else if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());

    let tracer = runtime.block_on(async move {
        opentelemetry_jaeger::new_pipeline()
            .with_agent_endpoint(std::env::var("JAEGER_COLLECTOR").unwrap())
            .with_service_name("foxbot_background_worker")
            .with_tags(vec![
                KeyValue::new("environment", env.to_owned()),
                KeyValue::new("version", env!("CARGO_PKG_VERSION")),
            ])
            .install_batch(opentelemetry::runtime::Tokio)
            .unwrap()
    });

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

    tracing::info!("starting channel worker");

    let workers: usize = config.background_workers.unwrap_or(4);

    tracing::debug!(workers, "got worker count configuration");

    let mut faktory = faktory::ConsumerBuilder::default();
    faktory.workers(workers);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&config.database_url);
    let pool = runtime
        .block_on(pool)
        .expect("unable to create database pool");

    let sites = runtime.block_on(foxbot_sites::get_all_sites(
        config.fa_a,
        config.fa_b,
        config.fautil_apitoken.clone(),
        config.weasyl_apitoken,
        config.twitter_consumer_key,
        config.twitter_consumer_secret,
        config.inkbunny_username,
        config.inkbunny_password,
        config.e621_login,
        config.e621_api_key,
        pool.clone(),
    ));

    let telegram = tgbotapi::Telegram::new(config.telegram_apitoken);
    let fuzzysearch = fuzzysearch::FuzzySearch::new(config.fautil_apitoken);

    let redis = redis::Client::open(config.redis_dsn).unwrap();
    let redis = runtime
        .block_on(redis::aio::ConnectionManager::new(redis))
        .expect("unable to open redis connection");

    let region = rusoto_core::Region::Custom {
        name: config.s3_region,
        endpoint: config.s3_endpoint,
    };

    let client = rusoto_core::request::HttpClient::new().unwrap();
    let provider =
        rusoto_credential::StaticProvider::new_minimal(config.s3_token, config.s3_secret);
    let s3 = rusoto_s3::S3Client::new_with(client, provider, region);

    let producer = faktory::Producer::connect(None).unwrap();

    let bot_user = runtime
        .block_on(telegram.make_request(&GetMe))
        .expect("unable to get own user");

    let handler = Arc::new(Handler {
        sites: tokio::sync::Mutex::new(sites),
        telegram: Arc::new(telegram),
        bot_user,
        producer: Arc::new(Mutex::new(producer)),
        fuzzysearch,
        conn: pool,
        redis,
        langs: load_langs(),
        best_langs: Default::default(),
        s3,
        config: config_clone,
    });

    let mut worker_environment = WorkerEnvironment::new(faktory, runtime, handler);

    worker_environment.register("channel_update", channel::process_channel_update);
    worker_environment.register("channel_edit", channel::process_channel_edit);
    worker_environment.register("group_photo", group::process_group_photo);
    worker_environment.register("group_source", group::process_group_source);
    worker_environment.register(
        "group_mediagroup_message",
        group::process_group_mediagroup_message,
    );
    worker_environment.register(
        "group_mediagroup_check",
        group::process_group_mediagroup_check,
    );
    worker_environment.register(
        "group_mediagroup_hash",
        group::process_group_mediagroup_hash,
    );
    worker_environment.register(
        "group_mediagroup_prune",
        group::process_group_mediagroup_prune,
    );
    worker_environment.register("hash_new", subscribe::process_hash_new);
    worker_environment.register("hash_notify", subscribe::process_hash_notify);

    let faktory = worker_environment.finalize();

    let faktory = faktory.connect(None).unwrap();
    faktory.run_to_completion(&["foxbot_background"]);
}

#[cfg(feature = "env")]
fn load_env() {
    dotenv::dotenv().unwrap();
}

#[cfg(not(feature = "env"))]
fn load_env() {}

struct WorkerEnvironment {
    faktory: faktory::ConsumerBuilder<Error>,
    runtime: Arc<tokio::runtime::Runtime>,
    handler: Arc<Handler>,
}

impl WorkerEnvironment {
    fn new(
        faktory: faktory::ConsumerBuilder<Error>,
        runtime: Arc<tokio::runtime::Runtime>,
        handler: Arc<Handler>,
    ) -> Self {
        Self {
            faktory,
            runtime,
            handler,
        }
    }

    fn register<F, Fut>(&mut self, name: &str, f: F)
    where
        F: 'static + Send + Sync + Fn(Arc<Handler>, faktory::Job) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let runtime = self.runtime.clone();
        let handler = self.handler.clone();

        self.faktory
            .register(name, move |job| -> Result<(), Error> {
                let span = get_custom_span(&job);

                runtime.block_on(f(handler.clone(), job).instrument(span))?;

                Ok(())
            });
    }

    fn finalize(self) -> faktory::ConsumerBuilder<Error> {
        self.faktory
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("telegram error: {0}")]
    Telegram(#[from] tgbotapi::Error),
    #[error("missing data")]
    MissingData,
    #[error("json error")]
    Json(#[from] serde_json::Error),
    #[error("other error: {0}")]
    Other(#[from] anyhow::Error),
}

type BestLangs = std::collections::HashMap<String, LangBundle>;

const MAX_SOURCE_DISTANCE: u64 = 3;
const NOISY_SOURCE_COUNT: usize = 4;

#[derive(serde::Deserialize, Debug, Clone)]
struct Config {
    // Site config
    fa_a: String,
    fa_b: String,
    weasyl_apitoken: String,
    inkbunny_username: String,
    inkbunny_password: String,
    e621_login: String,
    e621_api_key: String,

    // Twitter config
    twitter_consumer_key: String,
    twitter_consumer_secret: String,

    // Telegram config
    telegram_apitoken: String,

    // FuzzySearch config
    fautil_apitoken: String,

    // S3 compatible storage config
    s3_bucket: String,
    s3_endpoint: String,
    s3_region: String,
    s3_token: String,
    s3_secret: String,

    // Worker configuration
    background_workers: Option<usize>,
    database_url: String,
    redis_dsn: String,
    internet_url: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct MessageEdit {
    chat_id: String,
    message_id: i32,
    media_group_id: Option<String>,
    firsts: Vec<(Sites, String)>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct GroupSource {
    chat_id: String,
    reply_to_message_id: i32,
    text: String,
}

pub struct Handler {
    sites: tokio::sync::Mutex<Vec<BoxedSite>>,

    langs: Langs,
    best_langs: tokio::sync::RwLock<BestLangs>,
    config: Config,

    producer: Arc<Mutex<faktory::Producer<std::net::TcpStream>>>,
    telegram: Arc<tgbotapi::Telegram>,
    bot_user: tgbotapi::User,
    fuzzysearch: fuzzysearch::FuzzySearch,
    conn: sqlx::Pool<sqlx::Postgres>,
    redis: redis::aio::ConnectionManager,
    s3: rusoto_s3::S3Client,
}

impl Handler {
    /// Enqueue a new Faktory job by spawning a blocking task.
    async fn enqueue(&self, job: faktory::Job) {
        let producer = self.producer.clone();
        tokio::task::spawn_blocking(move || {
            let mut producer = producer.lock().unwrap();
            producer.enqueue(job).unwrap();
        });
    }

    /// Build a fluent language bundle for a specified language and cache the
    /// result.
    async fn get_fluent_bundle<C, R>(&self, requested: Option<&str>, callback: C) -> R
    where
        C: FnOnce(&fluent::concurrent::FluentBundle<fluent::FluentResource>) -> R,
    {
        let requested = requested.unwrap_or(L10N_LANGS[0]);

        tracing::trace!(lang = requested, "Looking up language bundle");

        {
            let lock = self.best_langs.read().await;
            if let Some(bundle) = lock.get(requested) {
                return callback(bundle);
            }
        }

        tracing::debug!(lang = requested, "Got new language, building bundle");

        let bundle = get_lang_bundle(&self.langs, requested);

        // Lock for writing for as short as possible.
        {
            let mut lock = self.best_langs.write().await;
            lock.insert(requested.to_string(), bundle);
        }

        let lock = self.best_langs.read().await;
        let bundle = lock.get(requested).expect("value just inserted is missing");
        callback(bundle)
    }
}

/// Set that chat needs additional time before another message can be sent.
#[tracing::instrument(skip(redis))]
pub async fn needs_more_time(
    redis: &redis::aio::ConnectionManager,
    chat_id: &str,
    at: chrono::DateTime<chrono::Utc>,
) {
    use redis::AsyncCommands;

    let mut redis = redis.clone();

    let now = chrono::Utc::now();
    let seconds = (at - now).num_seconds();

    if seconds <= 0 {
        tracing::warn!(seconds, "needed time already happened");
    }

    let key = format!("retry-at:{}", chat_id);
    if let Err(err) = redis
        .set_ex::<_, _, ()>(&key, at.timestamp(), seconds as usize)
        .await
    {
        tracing::error!("unable to set retry-at: {:?}", err);
    }
}

/// Check if a chat needs more time before a message can be sent.
#[tracing::instrument(skip(redis))]
pub async fn check_more_time(
    redis: &redis::aio::ConnectionManager,
    chat_id: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    use redis::AsyncCommands;

    let mut redis = redis.clone();

    let key = format!("retry-at:{}", chat_id);
    match redis.get::<_, Option<i64>>(&key).await {
        Ok(Some(timestamp)) => {
            let after = chrono::Utc.timestamp(timestamp, 0);
            if after <= chrono::Utc::now() {
                tracing::trace!("retry-at was in past, ignoring");
                None
            } else {
                Some(after)
            }
        }
        Ok(None) => None,
        Err(err) => {
            tracing::error!("unable to get retry-at: {:?}", err);

            None
        }
    }
}

fn get_custom_span(job: &faktory::Job) -> tracing::Span {
    let custom: HashMap<String, String> = job
        .custom
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|s| (key.to_owned(), s.to_owned())))
        .collect();
    let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&custom);

    let span = tracing::info_span!("faktory_job");
    span.set_parent(context);

    span
}
