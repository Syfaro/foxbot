use std::{collections::HashMap, sync::Arc};

use fluent_bundle::{bundle::FluentBundle, FluentArgs, FluentResource};
use fuzzysearch::FuzzySearch;
use intl_memoizer::concurrent::IntlLangMemoizer;
use lazy_static::lazy_static;
use prometheus::{
    register_counter_vec, register_histogram, register_histogram_vec, CounterVec, Histogram,
    HistogramVec,
};
use rand::prelude::SliceRandom;
use redis::AsyncCommands;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::Instrument;
use unic_langid::LanguageIdentifier;
use uuid::Uuid;

use crate::{
    services::{
        self,
        faktory::{FaktoryClient, FaktoryWorkerEnvironment, JobQueue},
        Telegram,
    },
    sites::BoxedSite,
    utils, Args, Error, ErrorMetadata, RunConfig, TelegramConfig, L10N_LANGS, L10N_RESOURCES,
};

use self::jobs::*;

mod handlers;
pub mod jobs;

type BoxedHandler = Box<dyn handlers::Handler + Send + Sync>;

lazy_static! {
    static ref PROCESSING_DURATION: Histogram = register_histogram!(
        "foxbot_processing_duration_seconds",
        "Time to run an update through handlers"
    )
    .unwrap();
    static ref HANDLER_DURATION: HistogramVec = register_histogram_vec!(
        "foxbot_handler_duration_seconds",
        "Time to complete to execute a completed handler",
        &["handler"]
    )
    .unwrap();
    static ref HANDLER_ERRORS: CounterVec = register_counter_vec!(
        "foxbot_handler_errors_total",
        "Number of errors when trying to run a handler",
        &["handler"]
    )
    .unwrap();
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

pub struct Config {
    pub sentry_url: Option<String>,
    pub sentry_organization_slug: Option<String>,
    pub sentry_project_slug: Option<String>,

    pub public_endpoint: String,

    pub s3_url: String,
    pub s3_bucket: String,

    pub twitter_callback: String,
    pub twitter_keypair: egg_mode::KeyPair,
}

pub async fn telegram(args: Args, config: RunConfig, telegram_config: TelegramConfig) {
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

    crate::run_migrations(&pool).await;

    let sites = crate::sites::get_all_sites(&config, pool.clone()).await;

    let faktory = FaktoryClient::connect(&config.faktory_url)
        .await
        .expect("could not connect to faktory");

    let redis = redis::Client::open(config.redis_url).expect("could not connect to redis");
    let redis = redis::aio::ConnectionManager::new(redis)
        .await
        .expect("could not create redis connection manager");

    let bot = Telegram::new(telegram_config.telegram_api_token);

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

    let app_config = Config {
        sentry_url: args.sentry_url.map(|dsn| dsn.to_string()),
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

        config: app_config,

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

    faktory_environment.register::<TelegramIngestJob, _, _, _>(|cx, _job, update| async move {
        process_update(&cx, update).await?;

        Ok(())
    });

    for handler in &cx.handlers {
        handler.add_jobs(&mut faktory_environment);
    }

    faktory_environment
        .register::<ChannelUpdateJob, _, _, _>(jobs::channel::process_channel_update);
    faktory_environment.register::<ChannelEditJob, _, _, _>(jobs::channel::process_channel_edit);
    faktory_environment.register::<GroupPhotoJob, _, _, _>(jobs::group::process_group_photo);
    faktory_environment.register::<GroupSourceJob, _, _, _>(jobs::group::process_group_source);
    faktory_environment
        .register::<MediaGroupMessageJob, _, _, _>(jobs::group::process_group_mediagroup_message);
    faktory_environment
        .register::<MediaGroupCheckJob, _, _, _>(jobs::group::process_group_mediagroup_check);
    faktory_environment
        .register::<MediaGroupHashJob, _, _, _>(jobs::group::process_group_mediagroup_hash);
    faktory_environment
        .register::<MediaGroupPruneJob, _, _, _>(jobs::group::process_group_mediagroup_prune);
    faktory_environment.register::<NewHashJob, _, _, _>(jobs::subscribe::process_hash_new);
    faktory_environment.register::<HashNotifyJob, _, _, _>(jobs::subscribe::process_hash_notify);

    let mut environment = faktory_environment.finalize();
    environment.workers(telegram_config.worker_threads);

    let mut tags = vec!["foxbot".to_string()];
    if telegram_config.high_priority {
        tags.push("high".to_string());
    }

    environment.labels(tags);

    let faktory = environment.connect(Some(&config.faktory_url)).unwrap();

    if telegram_config.high_priority {
        tracing::info!("only running high priority jobs");

        faktory.run_to_completion(&[TelegramJobQueue::HighPriority.as_str()]);
    } else {
        tracing::info!("running all jobs");

        faktory.run_to_completion(&TelegramJobQueue::priority_order());
    }
}

pub struct Context {
    handlers: Vec<BoxedHandler>,
    sites: Vec<BoxedSite>,

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

    async fn send_generic_reply(
        &self,
        message: &tgbotapi::Message,
        name: &str,
    ) -> Result<tgbotapi::Message, Error> {
        let bundle = self.get_fluent_bundle(message).await;

        let text = utils::get_message(&bundle, name, None);

        let send_message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            allow_sending_without_reply: Some(true),
            text,
            ..Default::default()
        };

        let message = self.bot.make_request(&send_message).await?;

        Ok(message)
    }

    async fn handle_welcome(
        &self,
        message: &tgbotapi::Message,
        command: &str,
    ) -> Result<(), Error> {
        let random_artwork = *STARTING_ARTWORK.choose(&mut rand::thread_rng()).unwrap();
        let bundle = self.get_fluent_bundle(message).await;

        let try_me = utils::get_message(&bundle, "welcome-try-me", None);

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

        let welcome = utils::get_message(&bundle, name, None);

        let send_message = tgbotapi::requests::SendMessage {
            chat_id: message.chat_id(),
            text: welcome,
            reply_markup: Some(reply_markup),
            ..Default::default()
        };

        self.bot.make_request(&send_message).await?;

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
                    utils::get_message(&bundle, "error-generic-count", Some(args))
                } else if has_user_message {
                    utils::get_message(&bundle, "error-generic-message", Some(args))
                } else {
                    utils::get_message(&bundle, "error-generic", None)
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

                utils::get_message(&bundle, name, Some(args))
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

            if let Err(err) = self.bot.make_request(&edit_message).await {
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

            match self.bot.make_request(&send_message).await {
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
    let processing_duration = PROCESSING_DURATION.start_timer();

    tracing::trace!("starting to process update");

    let user = utils::user_from_update(&update);
    let chat = utils::chat_from_update(&update);

    if let Some(user) = user {
        tracing::Span::current().record("user_id", &user.id);

        sentry::configure_scope(|scope| {
            scope.set_user(Some(sentry::User {
                id: Some(user.id.to_string()),
                username: user.username.clone(),
                ..Default::default()
            }));
        });
    }

    if let Some(chat) = chat {
        tracing::Span::current().record("chat_id", &chat.id);
    }

    let command = update
        .message
        .as_ref()
        .and_then(|message| message.get_command());

    for handler in &cx.handlers {
        let handler_duration = HANDLER_DURATION
            .with_label_values(&[handler.name()])
            .start_timer();

        match handler
            .handle(cx, &update, command.as_ref())
            .instrument(tracing::info_span!(
                "handler",
                handler_name = handler.name()
            ))
            .await
        {
            Ok(status) if status == handlers::Status::Completed => {
                tracing::debug!(
                    handled_by = handler.name(),
                    "completed update in {} seconds",
                    handler_duration.stop_and_record()
                );
                break;
            }
            Ok(_status) => {
                handler_duration.stop_and_discard();
            }
            Err(err) => {
                handler_duration.stop_and_record();
                HANDLER_ERRORS.with_label_values(&[handler.name()]).inc();

                if matches!(err, Error::UserMessage { .. }) {
                    tracing::warn!(handled_by = handler.name(), "user error: {:?}", err);
                } else {
                    tracing::error!(handled_by = handler.name(), "handler error: {:?}", err);
                }

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
        }
    }

    tracing::info!(
        "completed handler execution in {} seconds",
        processing_duration.stop_and_record()
    );

    // Errors aren't safe to retry because messages may have already been sent
    // to users.

    Ok(())
}

/// A convenience macro for handlers to ignore updates that don't contain a
/// required field.
#[macro_export]
macro_rules! needs_field {
    ($message:expr, $field:tt) => {
        match $message.$field {
            Some(ref field) => field,
            _ => return Ok($crate::execute::telegram::handlers::Status::Ignored),
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
