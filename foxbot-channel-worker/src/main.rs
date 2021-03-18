use anyhow::Context;
use std::{
    ops::Add,
    sync::{Arc, Mutex},
};
use tgbotapi::{
    requests::{EditMessageCaption, EditMessageReplyMarkup, ReplyMarkup},
    InlineKeyboardButton, InlineKeyboardMarkup,
};

use foxbot_sites::BoxedSite;
use foxbot_utils::*;

fn main() {
    tracing_subscriber::fmt::init();

    tracing::info!("Starting channel worker");

    load_env();
    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let workers: usize = std::env::var("CHANNEL_WORKERS")
        .as_deref()
        .unwrap_or("2")
        .parse()
        .unwrap_or(2);

    tracing::debug!(workers, "Got worker count configuration");

    let mut faktory = faktory::ConsumerBuilder::default();
    faktory.workers(workers);

    let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

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
        pool.clone(),
    ));

    let telegram = tgbotapi::Telegram::new(config.telegram_apitoken);
    let fuzzysearch = fuzzysearch::FuzzySearch::new(config.fautil_apitoken);

    let redis = redis::Client::open(config.redis_dsn).unwrap();
    let redis = runtime
        .block_on(redis::aio::ConnectionManager::new(redis))
        .expect("Unable to open Redis connection");

    let producer = faktory::Producer::connect(None).unwrap();

    let langs = load_langs();

    let handler = Arc::new(Handler {
        sites: tokio::sync::Mutex::new(sites),
        langs,
        telegram,
        producer: Arc::new(Mutex::new(producer)),
        fuzzysearch,
        conn: pool,
        redis,
    });

    {
        let runtime = runtime.clone();
        let handler = handler.clone();
        faktory.register(
            "foxbot_channel_update",
            move |job| -> Result<(), tgbotapi::Error> {
                let err = match runtime.block_on(handler.process_channel_update(job)) {
                    Ok(_) => return Ok(()),
                    Err(err) => err,
                };

                match err.downcast::<tgbotapi::Error>() {
                    Ok(err) => Err(err),
                    Err(err) => panic!("{:?}", err),
                }
            },
        );
    }

    faktory.register(
        "foxbot_channel_edit",
        move |job| -> Result<(), tgbotapi::Error> {
            let err = match runtime.block_on(handler.process_channel_edit(job)) {
                Ok(_) => return Ok(()),
                Err(err) => err,
            };

            match err.downcast::<tgbotapi::Error>() {
                Ok(err) => Err(err),
                Err(err) => panic!("{:?}", err),
            }
        },
    );

    let faktory = faktory.connect(None).unwrap();
    faktory.run_to_completion(&["foxbot_channel"]);
}

#[cfg(feature = "env")]
fn load_env() {
    dotenv::dotenv().unwrap();
}

#[cfg(not(feature = "env"))]
fn load_env() {}

#[derive(serde::Deserialize, Debug, Clone)]
struct Config {
    // Site config
    fa_a: String,
    fa_b: String,
    weasyl_apitoken: String,
    inkbunny_username: String,
    inkbunny_password: String,

    // Twitter config
    twitter_consumer_key: String,
    twitter_consumer_secret: String,

    // Telegram config
    telegram_apitoken: String,

    // FuzzySearch config
    fautil_apitoken: String,

    // Worker configuration
    channel_workers: Option<usize>,
    database_url: String,
    redis_dsn: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct MessageEdit {
    chat_id: i64,
    message_id: i32,
    media_group_id: Option<String>,
    file: fuzzysearch::File,
}

struct Handler {
    sites: tokio::sync::Mutex<Vec<BoxedSite>>,

    langs: Langs,

    producer: Arc<Mutex<faktory::Producer<std::net::TcpStream>>>,
    telegram: tgbotapi::Telegram,
    fuzzysearch: fuzzysearch::FuzzySearch,
    conn: sqlx::Pool<sqlx::Postgres>,
    redis: redis::aio::ConnectionManager,
}

impl Handler {
    async fn enqueue(&self, job: faktory::Job) {
        let producer = self.producer.clone();
        tokio::task::spawn_blocking(move || {
            let mut producer = producer.lock().unwrap();
            producer.enqueue(job).unwrap();
        });
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    async fn process_channel_update(&self, job: faktory::Job) -> anyhow::Result<()> {
        let data = job.args().iter().next().unwrap().to_owned();
        let message: tgbotapi::Message = serde_json::value::from_value(data).unwrap();

        tracing::trace!("Got enqueued message: {:?}", message);

        let sizes = match &message.photo {
            Some(photo) => photo,
            _ => return Ok(()),
        };

        let file = find_best_photo(&sizes).unwrap();
        let mut matches = match_image(
            &self.telegram,
            &self.conn,
            &self.fuzzysearch,
            &file,
            Some(3),
        )
        .await
        .context("unable to get matches")?;

        // Only keep matches with a distance of 3 or less
        matches.retain(|m| m.distance.unwrap() <= 3);

        let first = match matches.first() {
            Some(first) => first,
            _ => return Ok(()),
        };

        let sites = self.sites.lock().await;

        // If this link was already in the message, we can ignore it.
        if link_was_seen(&sites, &extract_links(&message), &first.url) {
            return Ok(());
        }

        drop(sites);

        if already_had_source(&self.redis, &message, &matches).await? {
            return Ok(());
        }

        let data = serde_json::to_value(&MessageEdit {
            chat_id: message.chat.id,
            message_id: message.message_id,
            media_group_id: message.media_group_id,
            file: first.to_owned(),
        })?;

        self.enqueue(
            faktory::Job::new("foxbot_channel_edit", vec![data]).on_queue("foxbot_channel"),
        )
        .await;

        Ok(())
    }

    #[tracing::instrument(skip(self, job), fields(job_id = job.id()))]
    async fn process_channel_edit(&self, job: faktory::Job) -> anyhow::Result<()> {
        let data: serde_json::Value = job.args().iter().next().unwrap().to_owned();
        tracing::trace!("Got enqueued edit: {:?}", data);

        let MessageEdit {
            chat_id,
            message_id,
            media_group_id,
            file,
        } = serde_json::value::from_value(data.clone()).unwrap();

        // If this photo was part of a media group, we should set a caption on
        // the image because we can't make an inline keyboard on it.
        let resp = if media_group_id.is_some() {
            let edit_caption_markup = EditMessageCaption {
                chat_id: chat_id.into(),
                message_id: Some(message_id),
                caption: Some(file.url()),
                ..Default::default()
            };

            self.telegram.make_request(&edit_caption_markup).await
        // Not a media group, we should create an inline keyboard.
        } else {
            let bundle = get_lang_bundle(&self.langs, L10N_LANGS[0]);
            let text = get_message(&bundle, "inline-source", None).unwrap();

            let markup = InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text,
                    url: Some(file.url()),
                    ..Default::default()
                }]],
            };

            let edit_reply_markup = EditMessageReplyMarkup {
                chat_id: chat_id.into(),
                message_id: Some(message_id),
                reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(markup)),
                ..Default::default()
            };

            self.telegram.make_request(&edit_reply_markup).await
        };

        match resp {
            // When we get rate limited, mark the job as successful and enqueue
            // it again after the retry after period.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                parameters:
                    Some(tgbotapi::ResponseParameters {
                        retry_after: Some(retry_after),
                        ..
                    }),
                ..
            })) => {
                tracing::warn!(retry_after, "Rate limiting, re-enqueuing");

                let mut job =
                    faktory::Job::new("foxbot_channel_edit", vec![data]).on_queue("foxbot_channel");
                let now = chrono::offset::Utc::now();
                let retry_at = now.add(chrono::Duration::seconds(retry_after as i64));
                job.at = Some(retry_at);
                self.enqueue(job).await;

                Ok(())
            }
            // It seems like the bot gets updates from channels, but is unable
            // to update them. Telegram often gives us a
            // 'Bad Request: MESSAGE_ID_INVALID' response here.
            //
            // The corresponding updates have inline keyboard markup, suggesting
            // that they were generated by a bot.
            //
            // I'm not sure if there's any way to detect this before processing
            // an update, so ignore these errors.
            Err(tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                error_code: Some(400),
                ..
            })) => Ok(()),
            Ok(_) => Ok(()),
            Err(e) => Err(e).context("unable to update channel message"),
        }
    }
}

/// Telegram only shows a caption on a media group if there is a single caption
/// anywhere in the group. When users upload a group, we need to check if we can
/// only set a single source to make the link more visible. This can be done by
/// ensuring our source has been previouly used in the media group.
///
/// For our implementation, this is done by maintaining a Redis set of every
/// source previously displayed. If adding our source links returns fewer
/// inserted than we had, it means a link was previously used and therefore we
/// do not have to set a source.
///
/// Because Telegram doesn't send media groups at once, we have to store these
/// values until we're sure the group is over. In this case, we will store
/// values for 300 seconds.
///
/// No link normalization is required here because all links are already
/// normalized when coming from FuzzySearch.
async fn already_had_source(
    conn: &redis::aio::ConnectionManager,
    message: &tgbotapi::Message,
    matches: &[fuzzysearch::File],
) -> anyhow::Result<bool> {
    use redis::AsyncCommands;

    let group_id = match &message.media_group_id {
        Some(id) => id,
        _ => return Ok(false),
    };

    let key = format!("group-sources:{}", group_id);

    let mut urls = matches.iter().map(|m| m.url()).collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    let source_count = urls.len();

    let mut conn = conn.clone();
    let added_links: usize = conn.sadd(&key, urls).await?;
    conn.expire(&key, 300).await?;

    tracing::debug!(
        source_count,
        added_links,
        "Determined existing and new source links"
    );

    Ok(source_count < added_links)
}
