use std::{collections::HashMap, sync::Arc, time::Duration};

use fluent_bundle::{bundle::FluentBundle, FluentArgs, FluentResource};
use fuzzysearch::FuzzySearch;
use intl_memoizer::concurrent::IntlLangMemoizer;
use rand::prelude::SliceRandom;
use redis::AsyncCommands;
use sqlx::PgPool;
use tgbotapi::{Telegram, TelegramRequest};
use tokio::sync::{Mutex, RwLock};
use tracing::Instrument;
use unic_langid::LanguageIdentifier;
use uuid::Uuid;

use crate::{
    services::{
        self,
        faktory::{FaktoryClient, FaktoryWorkerEnvironment},
    },
    sites::BoxedSite,
    utils, Args, Error, ErrorMetadata, RunConfig, TelegramConfig, L10N_LANGS, L10N_RESOURCES,
};

mod handlers;
mod jobs;

type BoxedHandler = Box<dyn handlers::Handler + Send + Sync>;

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

pub struct Config {
    pub sentry_url: String,
    pub sentry_organization_slug: String,
    pub sentry_project_slug: String,

    pub public_endpoint: String,

    pub s3_url: String,
    pub s3_bucket: String,

    pub twitter_callback: String,
    pub twitter_keypair: egg_mode::KeyPair,
}

pub fn start_telegram(args: Args, config: RunConfig, telegram_config: TelegramConfig) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("could not create runtime");

    rt.block_on(run_telegram(args, config, telegram_config))
}

async fn run_telegram(args: Args, config: RunConfig, telegram_config: TelegramConfig) {
    let mut dir = std::env::current_dir().expect("Unable to get directory");
    dir.push("langs");

    let mut langs = HashMap::with_capacity(L10N_LANGS.len());

    for lang in L10N_LANGS {
        let path = dir.join(lang);

        let mut lang_resources = Vec::with_capacity(L10N_RESOURCES.len());
        let langid = lang
            .parse::<LanguageIdentifier>()
            .expect("unable to parse language identifier");

        for resource in L10N_RESOURCES {
            let file = path.join(resource);
            let s = std::fs::read_to_string(file).expect("unable to read language file");

            lang_resources.push(s);
        }

        langs.insert(langid, lang_resources);
    }

    let pool = PgPool::connect(&config.database_url)
        .await
        .expect("could not connect to database");

    let sites = Mutex::new(super::get_sites(&pool, &config).await);

    let faktory = FaktoryClient::connect(&config.faktory_url)
        .await
        .expect("could not connect to faktory");

    let redis = redis::Client::open(config.redis_url).expect("could not connect to redis");
    let redis = redis::aio::ConnectionManager::new(redis)
        .await
        .expect("could not create redis connection manager");

    let bot = tgbotapi::Telegram::new(telegram_config.telegram_api_token);

    let bot_user = bot
        .make_request(&tgbotapi::requests::GetMe)
        .await
        .expect("could not get bot user");

    let fuzzysearch = fuzzysearch::FuzzySearch::new(config.fuzzysearch_api_token);

    let coconut = crate::services::coconut::Coconut::new(
        config.coconut_api_token,
        format!(
            "{}/coconut/{}",
            config.public_endpoint, config.coconut_secret
        ),
        config.b2_account_id,
        config.b2_app_key,
        config.b2_bucket_id,
    );

    let client =
        rusoto_core::request::HttpClient::new().expect("could not create rusoto http client");

    let provider =
        rusoto_credential::StaticProvider::new_minimal(config.s3_token, config.s3_secret);

    let region = rusoto_core::Region::Custom {
        name: config.s3_region,
        endpoint: config.s3_endpoint,
    };

    let s3 = rusoto_s3::S3Client::new_with(client, provider, region);

    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);

    let twitter_keypair =
        egg_mode::KeyPair::new(config.twitter_consumer_key, config.twitter_consumer_secret);

    let telegram_config = Config {
        sentry_url: args.sentry_url.to_string(),
        sentry_organization_slug: config.sentry_organization_slug,
        sentry_project_slug: config.sentry_project_slug,

        s3_bucket: config.s3_bucket,
        s3_url: config.s3_url,

        twitter_callback: format!("{}/twitter/callback", config.public_endpoint),
        twitter_keypair,

        public_endpoint: config.public_endpoint,
    };

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

    let cx = Arc::new(Context {
        sites,
        handlers,

        langs,
        best_lang: Default::default(),

        config: telegram_config,

        faktory,
        pool,
        redis,

        bot: Arc::new(bot),
        fuzzysearch: Arc::new(fuzzysearch),
        coconut: Arc::new(coconut),
        s3,
        finder,

        bot_user,
    });

    let mut faktory_environment: FaktoryWorkerEnvironment<_, Error> =
        FaktoryWorkerEnvironment::new(cx.clone());

    faktory_environment.register(crate::web::INGEST_TELEGRAM_JOB, |cx, job| async move {
        let mut args = job.args().iter();
        let (update,) = crate::extract_args!(args, tgbotapi::Update);

        process_update(&cx, update).await?;

        Ok(())
    });

    for handler in &cx.handlers {
        handler.add_jobs(&mut faktory_environment);
    }

    faktory_environment.register("channel_update", jobs::channel::process_channel_update);
    faktory_environment.register("channel_edit", jobs::channel::process_channel_edit);
    faktory_environment.register("group_photo", jobs::group::process_group_photo);
    faktory_environment.register("group_source", jobs::group::process_group_source);
    faktory_environment.register(
        "group_mediagroup_message",
        jobs::group::process_group_mediagroup_message,
    );
    faktory_environment.register(
        "group_mediagroup_check",
        jobs::group::process_group_mediagroup_check,
    );
    faktory_environment.register(
        "group_mediagroup_hash",
        jobs::group::process_group_mediagroup_hash,
    );
    faktory_environment.register(
        "group_mediagroup_prune",
        jobs::group::process_group_mediagroup_prune,
    );
    faktory_environment.register("hash_new", jobs::subscribe::process_hash_new);
    faktory_environment.register("hash_notify", jobs::subscribe::process_hash_notify);

    let environment = faktory_environment.finalize();

    let faktory = environment.connect(Some(&config.faktory_url)).unwrap();

    faktory.run_to_completion(&[
        crate::web::TELEGRAM_HIGH_PRIORITY_QUEUE,
        crate::web::TELEGRAM_STANDARD_QUEUE,
        crate::web::FOXBOT_DEFAULT_QUEUE,
    ]);
}

pub struct Context {
    handlers: Vec<BoxedHandler>,
    sites: Mutex<Vec<BoxedSite>>,

    langs: HashMap<LanguageIdentifier, Vec<String>>,
    best_lang: RwLock<HashMap<String, Arc<FluentBundle<FluentResource, IntlLangMemoizer>>>>,

    config: Config,

    faktory: FaktoryClient,
    pool: PgPool,
    redis: redis::aio::ConnectionManager,

    bot: Arc<Telegram>,
    fuzzysearch: Arc<FuzzySearch>,
    coconut: Arc<services::coconut::Coconut>,
    s3: rusoto_s3::S3Client,
    finder: linkify::LinkFinder,

    bot_user: tgbotapi::User,
}

pub trait LocaleSource {
    fn locale(&self) -> Option<&str>;
}

impl LocaleSource for Option<&str> {
    fn locale(&self) -> Option<&str> {
        self.to_owned()
    }
}

impl LocaleSource for &tgbotapi::User {
    fn locale(&self) -> Option<&str> {
        self.language_code.as_deref()
    }
}

impl LocaleSource for &tgbotapi::Message {
    fn locale(&self) -> Option<&str> {
        self.from
            .as_ref()
            .and_then(|user| user.language_code.as_deref())
    }
}

impl Context {
    #[tracing::instrument(skip(self, requested), fields(requested))]
    async fn get_fluent_bundle<R: LocaleSource>(
        &self,
        requested: R,
    ) -> Arc<FluentBundle<FluentResource, IntlLangMemoizer>> {
        let locale = requested.locale().unwrap_or(crate::L10N_LANGS[0]);
        tracing::Span::current().record("requested", &locale);

        tracing::trace!("looking up language bundle");

        {
            let lock = self.best_lang.read().await;

            if let Some(bundle) = lock.get(locale) {
                tracing::trace!("already computed best language");
                return bundle.clone();
            }
        }

        tracing::info!("got new language, building bundle");

        let bundle = Arc::new(utils::get_lang_bundle(&self.langs, locale));

        {
            let mut lock = self.best_lang.write().await;
            lock.insert(locale.to_string(), bundle.clone());
        }

        bundle
    }

    async fn make_request<T>(&self, request: &T) -> Result<T::Response, tgbotapi::Error>
    where
        T: TelegramRequest,
    {
        let mut attempts = 0;

        loop {
            let err = match self.bot.make_request(request).await {
                Ok(resp) => return Ok(resp),
                Err(err) => err,
            };

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
                }) => {
                    tracing::warn!(retry_after, "request was rate limited, retrying");
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
                    tracing::warn!("telegram network request error: {}", err);
                    2
                }
                err => {
                    tracing::warn!("got other telegram error: {}", err);
                    return Err(err);
                }
            };

            tokio::time::sleep(Duration::from_secs(retry_after as u64)).await;
            attempts += 1;
        }
    }

    async fn send_generic_reply(
        &self,
        message: &tgbotapi::Message,
        name: &str,
    ) -> Result<tgbotapi::Message, Error> {
        let bundle = self.get_fluent_bundle(message).await;

        let text = utils::get_message(&bundle, name, None).unwrap();

        let send_message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            allow_sending_without_reply: Some(true),
            text,
            ..Default::default()
        };

        let message = self.make_request(&send_message).await?;

        Ok(message)
    }

    async fn handle_welcome(
        &self,
        message: &tgbotapi::Message,
        command: &str,
    ) -> Result<(), Error> {
        let random_artwork = *STARTING_ARTWORK.choose(&mut rand::thread_rng()).unwrap();
        let bundle = self.get_fluent_bundle(message).await;

        let try_me = utils::get_message(&bundle, "welcome-try-me", None).unwrap();

        let reply_markup =
            tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: vec![vec![tgbotapi::InlineKeyboardButton {
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

        let welcome = utils::get_message(&bundle, name, None).unwrap();

        let send_message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            text: welcome,
            reply_markup: Some(reply_markup),
            ..Default::default()
        };

        self.make_request(&send_message).await?;

        Ok(())
    }

    async fn report_error<C>(
        &self,
        message: &tgbotapi::Message,
        err: Error,
        tags: Option<Vec<(&str, String)>>,
        callback: C,
    ) where
        C: FnOnce(&crate::Error) -> Uuid,
    {
        tracing::warn!("sending error: {:?}", err);

        let u = utils::with_user_scope(message.from.as_ref(), &err, tags, callback);

        let mut conn = self.redis.clone();

        let key_list = format!("errors:list:{}", message.chat.id);
        let key_message_id = format!("errors:message-id:{}", message.chat.id);

        let recent_error_count: i32 = conn.llen(&key_list).await.ok().unwrap_or(0);

        let _ = conn.lpush::<_, _, ()>(&key_list, u.to_string()).await;
        let _ = conn.expire::<_, ()>(&key_list, 60 * 5).await;
        let _ = conn.expire::<_, ()>(&key_message_id, 60 * 5).await;

        let bundle = self.get_fluent_bundle(message).await;
        let user_message = err.user_message();

        let msg = {
            let mut args = FluentArgs::new();
            args.set("count", recent_error_count + 1);

            let has_user_message = if let Some(message) = user_message {
                args.set("message", message);
                true
            } else {
                false
            };

            if u.is_nil() {
                if recent_error_count > 0 {
                    utils::get_message(&bundle, "error-generic-count", Some(args)).unwrap()
                } else if has_user_message {
                    utils::get_message(&bundle, "error-generic-message", Some(args)).unwrap()
                } else {
                    utils::get_message(&bundle, "error-generic", None).unwrap()
                }
            } else {
                let f = format!("`{}`", u);
                args.set("uuid", f);

                let name = if recent_error_count > 0 {
                    "error-uuid-count"
                } else if has_user_message {
                    "error-uuid-message"
                } else {
                    "error-uuid"
                };

                utils::get_message(&bundle, name, Some(args)).unwrap()
            }
        };

        let delete_markup = Some(tgbotapi::requests::ReplyMarkup::InlineKeyboardMarkup(
            tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: vec![vec![tgbotapi::InlineKeyboardButton {
                    text: "Delete".to_string(),
                    callback_data: Some("delete".to_string()),
                    ..Default::default()
                }]],
            },
        ));

        if recent_error_count > 0 {
            let message_id: i32 = match conn.get(&key_message_id).await {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!("could not retrieve message id to edit: {}", err);
                    return;
                }
            };

            let edit_message = tgbotapi::requests::EditMessageText {
                chat_id: message.chat_id(),
                message_id: Some(message_id),
                text: msg,
                parse_mode: Some(tgbotapi::requests::ParseMode::Markdown),
                reply_markup: delete_markup,
                ..Default::default()
            };

            if let Err(err) = self.make_request(&edit_message).await {
                tracing::error!("could not update error message: {}", err);

                let _ = conn.del::<_, ()>(&key_list).await;
                let _ = conn.del::<_, ()>(&key_message_id).await;
            }
        } else {
            let send_message = tgbotapi::requests::SendMessage {
                chat_id: message.chat_id(),
                text: msg,
                parse_mode: Some(tgbotapi::requests::ParseMode::Markdown),
                reply_to_message_id: Some(message.message_id),
                reply_markup: delete_markup,
                ..Default::default()
            };

            match self.make_request(&send_message).await {
                Ok(resp) => {
                    if let Err(err) = conn
                        .set_ex::<_, _, ()>(key_message_id, resp.message_id, 60 * 5)
                        .await
                    {
                        tracing::error!("unable to set redis error message id: {}", err);
                    }
                }
                Err(err) => {
                    tracing::error!("unable to send error message: {}", err);
                }
            }
        }
    }
}

#[tracing::instrument(skip(cx, update), fields(user_id, chat_id))]
async fn process_update(cx: &Context, update: tgbotapi::Update) -> Result<(), Error> {
    tracing::trace!("starting to process update");

    let user = utils::user_from_update(&update);
    let chat = utils::chat_from_update(&update);

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

    for handler in &cx.handlers {
        match handler
            .handle(cx, &update, command.as_ref())
            .instrument(tracing::info_span!(
                "handler",
                handler_name = handler.name()
            ))
            .await
        {
            Ok(status) if status == handlers::Status::Completed => {
                tracing::debug!(handled_by = handler.name(), "completed update");
                break;
            }
            Err(err) => {
                tracing::error!(handled_by = handler.name(), "handler error: {:?}", err);

                let mut tags = vec![("handler", handler.name().to_string())];
                if let Some(chat) = chat {
                    tags.push(("chat_id", chat.id.to_string()));
                }
                if let Some(command) = command {
                    tags.push(("command", command.name));
                }

                if let Some(message) = update.message {
                    cx.report_error(&message, err, Some(tags), sentry::capture_error)
                        .await;
                } else {
                    tracing::warn!("could not send error message to user");
                    sentry::capture_error(&err);
                }

                break;
            }
            _ => (),
        }
    }

    Ok(())
}

/// A convenience macro for handlers to ignore updates that don't contain a
/// required field.
#[macro_export]
macro_rules! needs_field {
    ($message:expr, $field:tt) => {
        match $message.$field {
            Some(ref field) => field,
            _ => return Ok(crate::execute::telegram::handlers::Status::Ignored),
        }
    };
}

/// Return early if something was an error or contained data.
#[macro_export]
macro_rules! potential_return {
    ($v:expr) => {
        match $v {
            Err(e) => return Err(e),
            Ok(Some(ret)) => return Ok(ret),
            _ => (),
        }
    };
}
