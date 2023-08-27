use std::{borrow::Cow, num::NonZeroU64};

use clap::{Parser, Subcommand};
use enum_map::Enum;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod execute;
mod models;
mod services;
mod sites;
mod utils;
mod web;

pub static L10N_RESOURCES: &[&str] = &["foxbot.ftl"];
pub static L10N_LANGS: &[&str] = &["en-US"];

#[derive(Clone, Debug, Parser)]
pub struct Args {
    #[clap(long, env)]
    pub sentry_url: Option<sentry::types::Dsn>,

    #[clap(long, env)]
    pub metrics_host: std::net::SocketAddr,
    #[clap(long, env)]
    pub unleash_host: String,
    #[clap(long, env)]
    pub unleash_secret: String,

    #[clap(subcommand)]
    pub command: Command,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Web server used for displaying content to users and receiving webhooks.
    Web(Box<WebConfig>),
    /// Run bot for a given site.
    Run(Box<RunConfig>),
}

#[derive(Clone, Debug, Subcommand)]
pub enum RunService {
    /// Run bot for Telegram.
    Telegram(TelegramConfig),
    /// Run bot for Discord.
    Discord(DiscordConfig),
    /// Run bot for Reddit.
    Reddit(RedditConfig),
}

#[derive(Clone, Debug, Parser)]
pub struct WebConfig {
    /// The base address where the ingest service is available.
    #[clap(long, env)]
    pub public_endpoint: String,

    /// Address to listen for incoming requests.
    #[clap(long, env)]
    pub http_host: String,

    /// Telegram API token, as provided by Botfather.
    #[clap(long, env)]
    pub telegram_api_token: String,

    /// URL for Faktory instance.
    #[clap(long, env)]
    pub faktory_url: String,

    /// Shared secret for Coconut.
    #[clap(long, env)]
    pub coconut_secret: String,

    /// API key for FuzzySearch.
    #[clap(long, env)]
    pub fuzzysearch_api_key: String,

    /// CDN prefix.
    #[clap(long, env)]
    pub cdn_prefix: String,

    /// PostgreSQL DSN.
    #[clap(long, env)]
    pub database_url: String,
    /// URL for Redis.
    #[clap(long, env)]
    pub redis_url: String,

    /// Twitter consumer key.
    #[clap(long, env)]
    pub twitter_consumer_key: String,

    /// Twitter consumer secret.
    #[clap(long, env)]
    pub twitter_consumer_secret: String,

    /// Cookie secret for management web UI, encoded as hex.
    /// Base URL of Fider instance for feedback.
    #[clap(long, env)]
    pub feedback_base: String,

    /// Fider OAuth URL.
    #[clap(long, env)]
    pub fider_oauth_url: String,

    /// JWT secret for feedback login.
    #[clap(long, env)]
    pub jwt_secret: String,

    /// Session secret token.
    #[clap(long, env)]
    pub session_secret: String,
}

#[derive(Clone, Debug, Parser)]
pub struct RunConfig {
    /// PostgreSQL DSN.
    #[clap(long, env)]
    pub database_url: String,
    /// URL for Faktory instance.
    #[clap(long, env)]
    pub faktory_url: String,
    /// URL for Redis.
    #[clap(long, env)]
    pub redis_url: String,

    /// Sentry organization slug, for error feedback.
    #[clap(long, env)]
    pub sentry_organization_slug: Option<String>,
    /// Sentry project slug, for error feedback.
    #[clap(long, env)]
    pub sentry_project_slug: Option<String>,

    /// The base address where the ingest service is available.
    #[clap(long, env)]
    pub public_endpoint: String,

    /// Endpoint for FuzzySearch.
    #[clap(long, env)]
    pub fuzzysearch_endpoint: Option<String>,
    /// API token for FuzzySearch.
    #[clap(long, env)]
    pub fuzzysearch_api_token: String,

    /// Shared secret for Coconut.
    #[clap(long, env)]
    pub coconut_secret: String,

    /// B2 account ID for video storage.
    #[clap(long, env)]
    pub b2_account_id: String,
    /// B2 app key for video storage.
    #[clap(long, env)]
    pub b2_app_key: String,
    /// B2 bucket ID for video storage.
    #[clap(long, env)]
    pub b2_bucket_id: String,

    /// S3 endpoint for image storage.
    #[clap(long, env)]
    pub s3_endpoint: String,
    /// S3 region for image storage.
    #[clap(long, env)]
    pub s3_region: String,
    /// S3 bucket for image storage.
    #[clap(long, env)]
    pub s3_bucket: String,
    /// S3 URL prefix for image storage.
    #[clap(long, env)]
    pub s3_url: String,
    /// S3 access token.
    #[clap(long, env)]
    pub s3_token: String,
    /// S3 secret token.
    #[clap(long, env)]
    pub s3_secret: String,

    /// API token for Coconut.
    #[clap(long, env)]
    pub coconut_api_token: String,

    /// FurAffinity login cookie a.
    #[clap(long, env)]
    pub furaffinity_cookie_a: String,
    /// FurAffinity login cookie b.
    #[clap(long, env)]
    pub furaffinity_cookie_b: String,

    /// Weasyl API token.
    #[clap(long, env)]
    pub weasyl_api_token: String,

    /// Twitter consumer key.
    #[clap(long, env)]
    pub twitter_consumer_key: String,
    /// Twitter consumer secret.
    #[clap(long, env)]
    pub twitter_consumer_secret: String,
    /// Twitter access key.
    ///
    /// This may be left empty to use app auth. Requires setting the Twitter
    /// access secret.
    #[clap(long, env, requires = "twitter_access_secret")]
    pub twitter_access_key: Option<String>,
    /// Twitter access secret.
    #[clap(long, env)]
    pub twitter_access_secret: Option<String>,
    /// Disable Twitter logins.
    #[clap(long, env, default_value = "false")]
    pub twitter_disable: bool,

    /// Inkbunny username.
    #[clap(long, env)]
    pub inkbunny_username: String,
    /// Inkbunny password.
    #[clap(long, env)]
    pub inkbunny_password: String,

    /// E621 login.
    #[clap(long, env)]
    pub e621_login: String,
    /// E621 API token.
    #[clap(long, env)]
    pub e621_api_token: String,

    /// Tumblr API key.
    #[clap(long, env)]
    pub tumblr_api_key: String,

    /// The type of service to run.
    #[clap(subcommand)]
    service: RunService,
}

#[derive(Clone, Debug, Parser)]
pub struct TelegramConfig {
    /// Telegram API token, as provided by Botfather.
    #[clap(long, env)]
    pub telegram_api_token: String,

    /// Number of workers to process jobs.
    #[clap(long, env, default_value = "2")]
    pub worker_threads: usize,

    /// If this worker instance should only run high priority updates.
    #[clap(long, env)]
    pub high_priority: bool,
}

type CommandId = twilight_model::id::Id<twilight_model::id::marker::CommandMarker>;

#[derive(Clone, Debug, Parser)]
pub struct DiscordConfig {
    /// Discord API token.
    #[clap(long, env)]
    pub discord_token: String,
    /// Discord application ID.
    #[clap(long, env)]
    pub discord_application_id: NonZeroU64,
    /// ID of registered find source command.
    #[clap(long, env)]
    pub find_source_command: CommandId,
    /// ID of registered share images command.
    #[clap(long, env)]
    pub share_images_command: CommandId,
}

#[derive(Clone, Debug, Parser)]
pub struct RedditConfig {
    /// Reddit OAuth client ID.
    #[clap(long, env)]
    pub reddit_client_id: String,
    /// Reddit OAuth client secret.
    #[clap(long, env)]
    pub reddit_client_secret: String,
    /// Reddit account username.
    #[clap(long, env)]
    pub reddit_username: String,
    /// Reddit account password.
    #[clap(long, env)]
    pub reddit_password: String,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("faktory error: {0}")]
    Faktory(#[from] crate::services::faktory::FaktoryError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("twitter error: {0}")]
    Twitter(#[from] egg_mode::error::Error),
    #[error("telegram error: {0}")]
    Telegram(#[from] tgbotapi::Error),
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("image error: {0}")]
    Image(#[from] image::error::ImageError),
    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("s3 error: {0}")]
    S3(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("discord http error: {0}")]
    DiscordHttp(#[from] twilight_http::Error),
    #[error("reddit error: {0}")]
    Reddit(#[from] roux::util::error::RouxError),

    #[error("bot issue: {0}")]
    Bot(Cow<'static, str>),
    #[error("limit exceeded: {0}")]
    Limit(Cow<'static, str>),
    #[error("essential data was missing: {0}")]
    Missing(Cow<'static, str>),

    #[error("{message}")]
    UserMessage {
        message: Cow<'static, str>,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl Error {
    pub fn bot<M>(message: M) -> Self
    where
        M: Into<Cow<'static, str>>,
    {
        Self::Bot(message.into())
    }

    pub fn limit<M>(name: M) -> Self
    where
        M: Into<Cow<'static, str>>,
    {
        Self::Limit(name.into())
    }

    pub fn missing<M>(data: M) -> Self
    where
        M: Into<Cow<'static, str>>,
    {
        Self::Missing(data.into())
    }

    pub fn user_message<M>(message: M) -> Self
    where
        M: Into<Cow<'static, str>>,
    {
        Self::UserMessage {
            message: message.into(),
            source: None,
        }
    }

    pub fn user_message_with_error<M, S>(message: M, source: S) -> Self
    where
        M: Into<Cow<'static, str>>,
        S: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        Self::UserMessage {
            message: message.into(),
            source: Some(source.into()),
        }
    }
}

impl<E: 'static + std::error::Error + Send + Sync> From<rusoto_core::RusotoError<E>> for Error {
    fn from(err: rusoto_core::RusotoError<E>) -> Self {
        Self::S3(Box::new(err))
    }
}

pub trait ErrorMetadata {
    fn is_retryable(&self) -> bool;
    fn user_message(&self) -> Option<&str>;
}

impl ErrorMetadata for Error {
    fn is_retryable(&self) -> bool {
        matches!(self, Error::Missing(_))
    }

    fn user_message(&self) -> Option<&str> {
        match self {
            Self::UserMessage { message, .. } => Some(message),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Enum, Serialize, Deserialize)]
pub enum Features {
    #[serde(rename = "foxbot_twitter_api")]
    TwitterApi,
    #[serde(rename = "foxbot_manage_web")]
    ManageWeb,
}

#[tokio::main]
async fn main() {
    #[cfg(feature = "dotenv")]
    dotenv::dotenv().expect("could not load dotenv");

    let args = Args::parse();

    let service_name = match &args.command {
        Command::Web(_) => "foxbot-web",
        Command::Run(run) => match run.service {
            RunService::Discord(_) => "foxbot-discord",
            RunService::Telegram(_) => "foxbot-telegram",
            RunService::Reddit(_) => "foxbot-reddit",
        },
    };

    foxlib::trace::init(foxlib::trace::TracingConfig {
        namespace: "foxbot",
        name: service_name,
        version: env!("CARGO_PKG_VERSION"),
        otlp: matches!(std::env::var("LOG_FMT").as_deref(), Ok("json")),
    });

    let _sentry = sentry::init(sentry::ClientOptions {
        dsn: args.sentry_url.clone(),
        release: sentry::release_name!(),
        attach_stacktrace: true,
        session_mode: sentry::SessionMode::Request,
        ..Default::default()
    });

    foxlib::MetricsServer::serve(args.metrics_host, true).await;

    match args.command.clone() {
        Command::Web(config) => web::web(args, *config).await,
        Command::Run(run_config) => match run_config.service.clone() {
            RunService::Telegram(telegram_config) => {
                execute::start_telegram(args, *run_config, telegram_config).await
            }
            RunService::Discord(discord_config) => {
                execute::start_discord(args, *run_config, discord_config).await
            }
            RunService::Reddit(reddit_config) => {
                execute::start_reddit(*run_config, reddit_config).await
            }
        },
    }
}

async fn run_migrations(pool: &sqlx::PgPool) {
    sqlx::migrate!()
        .run(pool)
        .await
        .expect("could not run migrations");
}
