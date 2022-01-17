use std::{borrow::Cow, num::NonZeroU64};

use clap::{Parser, Subcommand};
use thiserror::Error;
use tracing_subscriber::prelude::*;

mod execute;
mod models;
mod services;
mod sites;
mod types;
mod utils;
mod web;

pub static L10N_RESOURCES: &[&str] = &["foxbot.ftl"];
pub static L10N_LANGS: &[&str] = &["en-US"];

#[derive(Clone, Debug, Parser)]
pub struct Args {
    #[clap(long, env)]
    pub sentry_url: sentry::types::Dsn,

    #[clap(subcommand)]
    pub command: Command,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    Web(Box<WebConfig>),
    Run(Box<RunConfig>),
}

#[derive(Clone, Debug, Subcommand)]
pub enum RunService {
    Telegram(TelegramConfig),
    Discord(DiscordConfig),
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

    /// Twitter consumer key.
    #[clap(long, env)]
    pub twitter_consumer_key: String,

    /// Twitter consumer secret.
    #[clap(long, env)]
    pub twitter_consumer_secret: String,
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

    #[clap(long, env)]
    pub sentry_organization_slug: String,
    #[clap(long, env)]
    pub sentry_project_slug: String,

    /// The base address where the ingest service is available.
    #[clap(long, env)]
    pub public_endpoint: String,

    #[clap(long, env)]
    pub fuzzysearch_api_token: String,

    /// Shared secret for Coconut.
    #[clap(long, env)]
    pub coconut_secret: String,

    #[clap(long, env)]
    pub b2_account_id: String,
    #[clap(long, env)]
    pub b2_app_key: String,
    #[clap(long, env)]
    pub b2_bucket_id: String,

    #[clap(long, env)]
    pub s3_endpoint: String,
    #[clap(long, env)]
    pub s3_region: String,
    #[clap(long, env)]
    pub s3_bucket: String,
    #[clap(long, env)]
    pub s3_url: String,
    #[clap(long, env)]
    pub s3_token: String,
    #[clap(long, env)]
    pub s3_secret: String,

    #[clap(long, env)]
    pub coconut_api_token: String,

    #[clap(long, env)]
    pub furaffinity_cookie_a: String,
    #[clap(long, env)]
    pub furaffinity_cookie_b: String,

    #[clap(long, env)]
    pub weasyl_api_token: String,

    #[clap(long, env)]
    pub twitter_consumer_key: String,
    #[clap(long, env)]
    pub twitter_consumer_secret: String,

    #[clap(long, env)]
    pub inkbunny_username: String,
    #[clap(long, env)]
    pub inkbunny_password: String,

    #[clap(long, env)]
    pub e621_login: String,
    #[clap(long, env)]
    pub e621_api_token: String,

    #[clap(subcommand)]
    service: RunService,
}

#[derive(Clone, Debug, Parser)]
pub struct TelegramConfig {
    /// Telegram API token, as provided by Botfather.
    #[clap(long, env)]
    pub telegram_api_token: String,
}

#[derive(Clone, Debug, Parser)]
pub struct DiscordConfig {
    #[clap(long, env)]
    pub discord_token: String,
    #[clap(long, env)]
    pub discord_application_id: NonZeroU64,
    #[clap(long, env)]
    pub find_source_command: NonZeroU64,
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

    #[error("bot issue: {0}")]
    Bot(Cow<'static, str>),
    #[error("limit exceeded: {0}")]
    Limit(Cow<'static, str>),
    #[error("essential data was missing")]
    Missing,

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
        matches!(self, Error::Missing)
    }

    fn user_message(&self) -> Option<&str> {
        match self {
            Self::UserMessage { message, .. } => Some(message),
            _ => None,
        }
    }
}

fn main() {
    #[cfg(feature = "dotenv")]
    dotenv::dotenv().expect("could not load dotenv");

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_tracing::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let _sentry = sentry::init(sentry::ClientOptions {
        dsn: Some(args.sentry_url.clone()),
        release: sentry::release_name!(),
        attach_stacktrace: true,
        session_mode: sentry::SessionMode::Request,
        ..Default::default()
    });

    match args.command.clone() {
        Command::Web(config) => web::web(*config),
        Command::Run(run_config) => match run_config.service.clone() {
            RunService::Telegram(telegram_config) => {
                execute::telegram::start_telegram(args, *run_config, telegram_config)
            }
            RunService::Discord(discord_config) => {
                execute::discord::start_discord(*run_config, discord_config)
            }
        },
    }
}
