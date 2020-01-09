#![feature(try_trait)]

use sentry::integrations::failure::capture_fail;
use sites::{PostInfo, Site};
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc};
use telegram::*;
use tokio::sync::Mutex;
use tokio01::runtime::current_thread::block_on_all;
use unic_langid::LanguageIdentifier;

#[macro_use]
extern crate failure;

mod sites;

fn generate_id() -> String {
    use rand::Rng;
    let rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .collect()
}

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

#[tokio::main]
async fn main() {
    use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};

    pretty_env_logger::init();

    let (fa_a, fa_b) = (
        std::env::var("FA_A").expect("Missing FA token a"),
        std::env::var("FA_B").expect("Missing FA token b"),
    );

    let (consumer_key, consumer_secret) = (
        std::env::var("TWITTER_CONSUMER_KEY").expect("Missing Twitter consumer key"),
        std::env::var("TWITTER_CONSUMER_SECRET").expect("Missing Twitter consumer secret"),
    );

    let db_path = std::env::var("TWITTER_DATABASE").expect("Missing Twitter database path");
    let db = PickleDb::load(
        db_path.clone(),
        PickleDbDumpPolicy::AutoDump,
        SerializationMethod::Json,
    )
    .unwrap_or_else(|_| {
        PickleDb::new(
            db_path,
            PickleDbDumpPolicy::AutoDump,
            SerializationMethod::Json,
        )
    });

    let fa_util_api = std::env::var("FAUTIL_APITOKEN").expect("Missing FA Utility API token");
    let fapi = Arc::new(fautil::FAUtil::new(fa_util_api.clone()));

    let sites: Vec<Box<dyn Site + Send + Sync>> = vec![
        Box::new(sites::E621::new()),
        Box::new(sites::FurAffinity::new((fa_a, fa_b), fa_util_api)),
        Box::new(sites::Weasyl::new(
            std::env::var("WEASYL_APITOKEN").expect("Missing Weasyl API token"),
        )),
        Box::new(sites::Twitter::new(
            consumer_key.clone(),
            consumer_secret.clone(),
        )),
        Box::new(sites::Mastodon::new()),
        Box::new(sites::Direct::new(fapi.clone())),
    ];

    let bot = Arc::new(Telegram::new(
        std::env::var("TELEGRAM_APITOKEN").expect("Missing Telegram API token"),
    ));

    let (influx_host, influx_db) = (
        std::env::var("INFLUX_HOST").expect("Missing InfluxDB host"),
        std::env::var("INFLUX_DB").expect("Missing InfluxDB database"),
    );

    let (influx_user, influx_pass) = (
        std::env::var("INFLUX_USER").expect("Missing InfluxDB user"),
        std::env::var("INFLUX_PASS").expect("Missing InfluxDB password"),
    );

    let influx = influxdb::Client::new(influx_host, influx_db).with_auth(influx_user, influx_pass);

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

    let use_proxy = std::env::var("USE_PROXY")
        .unwrap_or_else(|_| "false".into())
        .parse()
        .unwrap_or(false);

    let bot_user = bot
        .make_request(&GetMe {})
        .await
        .expect("Unable to fetch bot user");

    let handler = Arc::new(Mutex::new(MessageHandler {
        sites,
        bot: bot.clone(),
        bot_user,
        finder,
        fapi,
        db,
        consumer_key,
        consumer_secret,
        langs,
        best_lang: HashMap::new(),
        influx,
        use_proxy,
    }));

    let use_webhooks: bool = std::env::var("USE_WEBHOOKS")
        .unwrap_or_else(|_| "false".to_string())
        .parse()
        .unwrap_or(false);

    let _guard = if let Ok(dsn) = std::env::var("SENTRY_DSN") {
        sentry::integrations::panic::register_panic_handler();
        Some(sentry::init(dsn))
    } else {
        None
    };

    if use_webhooks {
        let webhook_endpoint = std::env::var("WEBHOOK_ENDPOINT").expect("Missing WEBHOOK_ENDPOINT");
        let set_webhook = SetWebhook {
            url: webhook_endpoint,
        };
        if let Err(e) = bot.make_request(&set_webhook).await {
            capture_fail(&e);
            log::error!("Unable to set webhook: {:?}", e);
        }
        receive_webhook(handler).await;
    } else {
        let delete_webhook = DeleteWebhook {};
        if let Err(e) = bot.make_request(&delete_webhook).await {
            capture_fail(&e);
            log::error!("Unable to clear webhook: {:?}", e);
        }
        poll_updates(bot, handler).await;
    }
}

async fn handle_request(
    req: hyper::Request<hyper::Body>,
    handler: Arc<Mutex<MessageHandler>>,
    secret: &str,
) -> hyper::Result<hyper::Response<hyper::Body>> {
    use hyper::{Body, Response, StatusCode};

    let now = std::time::Instant::now();

    log::trace!("Got HTTP request: {} {}", req.method(), req.uri().path());

    match (req.method(), req.uri().path()) {
        (&hyper::Method::POST, path) if path == secret => {
            log::trace!("Handling update");
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

            {
                let mut handler = handler.lock().await;
                log::debug!("Got update: {:?}", update);
                handler.handle_message(update).await;
            }

            tokio::spawn(async move {
                let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "http")
                    .add_field("duration", now.elapsed().as_millis() as i64);

                let handler = handler.lock().await;
                if let Err(e) = handler.influx.query(&point).await {
                    let uuid = capture_fail(&e);
                    log::error!(
                        "[{}]  Unable to send http request info to InfluxDB: {:?}",
                        uuid,
                        e
                    );
                }
            });

            Ok(Response::new(Body::from("✓")))
        }
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

async fn receive_webhook(handler: Arc<Mutex<MessageHandler>>) {
    let host = std::env::var("HTTP_HOST").expect("Missing HTTP_HOST");
    let addr = host.parse().expect("Invalid HTTP_HOST");

    let secret_path = format!(
        "/{}",
        std::env::var("HTTP_SECRET").expect("Missing HTTP_SECRET")
    );
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

    log::info!("Listening on http://{}", addr);

    if let Err(e) = server.await {
        log::error!("Server error: {:?}", e);
    }
}

async fn poll_updates(bot: Arc<Telegram>, handler: Arc<Mutex<MessageHandler>>) {
    let mut update_req = GetUpdates::default();
    update_req.timeout = Some(30);

    loop {
        let updates = match bot.make_request(&update_req).await {
            Ok(updates) => updates,
            Err(e) => {
                capture_fail(&e);
                log::error!("Unable to get updates: {:?}", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        for update in updates {
            let id = update.update_id;

            let handler = handler.clone();

            tokio::spawn(async move {
                let mut handler = handler.lock().await;
                handler.handle_message(update).await
            });

            update_req.offset = Some(id + 1);
        }
    }
}

struct MessageHandler {
    sites: Vec<Box<dyn Site + Send + Sync>>,
    bot: Arc<Telegram>,
    bot_user: User,
    fapi: Arc<fautil::FAUtil>,
    finder: linkify::LinkFinder,
    db: pickledb::PickleDb,
    consumer_key: String,
    consumer_secret: String,
    langs: HashMap<LanguageIdentifier, Vec<String>>,
    best_lang: HashMap<String, fluent::FluentBundle<fluent::FluentResource>>,
    influx: influxdb::Client,
    use_proxy: bool,
}

fn get_message(
    bundle: &fluent::FluentBundle<fluent::FluentResource>,
    name: &str,
    args: Option<fluent::FluentArgs>,
) -> Result<String, Vec<fluent::FluentError>> {
    let msg = bundle.get_message(name).expect("Message doesn't exist");
    let pattern = msg.value.expect("Message has no value");
    let mut errors = vec![];
    let value = bundle.format_pattern(&pattern, args.as_ref(), &mut errors);
    if errors.is_empty() {
        Ok(value.to_string())
    } else {
        Err(errors)
    }
}

type SentryTags<'a> = Option<Vec<(&'a str, String)>>;

fn with_user_scope<C, R>(from: Option<&User>, tags: SentryTags, callback: C) -> R
where
    C: FnOnce() -> R,
{
    sentry::with_scope(
        |scope| {
            if let Some(user) = from {
                scope.set_user(Some(sentry::User {
                    id: Some(user.id.to_string()),
                    username: user.username.clone(),
                    ..Default::default()
                }));
            };

            if let Some(tags) = tags {
                for tag in tags {
                    scope.set_tag(tag.0, tag.1);
                }
            }
        },
        callback,
    )
}

impl MessageHandler {
    fn get_fluent_bundle(
        &mut self,
        requested: Option<&str>,
    ) -> &fluent::FluentBundle<fluent::FluentResource> {
        let requested = if let Some(requested) = requested {
            requested
        } else {
            "en-US"
        };

        log::trace!("Looking up language bundle for {}", requested);

        if self.best_lang.contains_key(requested) {
            return self
                .best_lang
                .get(requested)
                .expect("Should have contained");
        }

        log::info!("Got new language {}, building bundle", requested);

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

        self.best_lang.insert(requested.to_string(), bundle);
        self.best_lang
            .get(requested)
            .expect("Value just inserted is missing")
    }

    async fn report_error<C>(
        &mut self,
        message: &Message,
        tags: Option<Vec<(&str, String)>>,
        callback: C,
    ) where
        C: FnOnce() -> uuid::Uuid,
    {
        let u = with_user_scope(message.from.as_ref(), tags, callback);

        let lang_code = message
            .from
            .clone()
            .map(|from| from.language_code)
            .flatten();
        let bundle = self.get_fluent_bundle(lang_code.as_deref());

        let msg = if u.is_nil() {
            get_message(&bundle, "error-generic", None)
        } else {
            let f = format!("`{}`", u.to_string());

            let mut args = fluent::FluentArgs::new();
            args.insert("uuid", fluent::FluentValue::from(f));

            get_message(&bundle, "error-uuid", Some(args))
        };

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: msg.unwrap(),
            parse_mode: Some(ParseMode::Markdown),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&send_message).await {
            log::error!("Unable to send error message to user: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }

    async fn handle_inline(&mut self, inline: InlineQuery) {
        let now = std::time::Instant::now();

        let links: Vec<_> = self.finder.links(&inline.query).collect();
        let mut results: Vec<PostInfo> = Vec::new();

        log::info!("Got query: {}", inline.query);
        log::debug!("Found links: {:?}", links);

        let mut point = influxdb::Query::write_query(influxdb::Timestamp::Now, "inline");

        'link: for link in links {
            for site in &mut self.sites {
                let link_str = link.as_str();

                if site.url_supported(link_str).await {
                    log::debug!("Link {} supported by {}", link_str, site.name());

                    let images = match site.get_images(inline.from.id, link_str).await {
                        Ok(images) => images,
                        Err(e) => {
                            log::warn!("Unable to get image: {:?}", e);
                            with_user_scope(
                                Some(&inline.from),
                                Some(vec![("site", site.name().to_string())]),
                                || {
                                    capture_fail(&e);
                                },
                            );
                            continue 'link;
                        }
                    };

                    let images = match images {
                        Some(images) => images,
                        None => continue 'link,
                    };

                    log::debug!("Found images: {:?}", images);
                    point = point.add_tag("site", site.name().replace(" ", "_"));

                    results.extend(images);

                    continue 'link;
                }
            }
        }

        let personal = results.iter().any(|result| result.personal);

        let mut responses: Vec<InlineQueryResult> = results
            .iter()
            .map(|result| self.process_result(result, &inline.from))
            .filter_map(|result| result)
            .flatten()
            .collect();

        point = point.add_field("count", responses.len() as i32);

        if responses.is_empty() {
            let bundle = self.get_fluent_bundle(inline.from.language_code.as_deref());

            if !inline.query.is_empty() {
                responses.push(InlineQueryResult::article(
                    generate_id(),
                    get_message(&bundle, "inline-no-results-title", None).unwrap(),
                    get_message(&bundle, "inline-no-results-body", None).unwrap(),
                ));
            }
        }

        let mut answer_inline = AnswerInlineQuery {
            inline_query_id: inline.id,
            results: responses,
            is_personal: Some(personal),
            ..Default::default()
        };

        if inline.query.is_empty() {
            answer_inline.switch_pm_text = Some("Help".to_string());
            answer_inline.switch_pm_parameter = Some("help".to_string());
        }

        if let Err(e) = self.bot.make_request(&answer_inline).await {
            with_user_scope(Some(&inline.from), None, || {
                capture_fail(&e);
            });
            log::error!("Unable to respond to inline: {:?}", e);
        }

        point = point.add_field("duration", now.elapsed().as_millis() as i64);
        if let Err(e) = self.influx.query(&point).await {
            log::error!("Unable to log inline to InfluxDB: {:?}", e);
            with_user_scope(Some(&inline.from), None, || capture_fail(&e));
        };
    }

    async fn handle_command(&mut self, message: Message) {
        let now = std::time::Instant::now();

        let entities = message.clone().entities.unwrap(); // only here because this existed

        let command = entities
            .iter()
            .find(|entity| entity.entity_type == MessageEntityType::BotCommand);
        let command = match command {
            Some(command) => command,
            None => return,
        };
        let text = message.text.clone().unwrap();
        let command_text: String = text
            .chars()
            .skip(command.offset as usize)
            .take(command.length as usize)
            .collect();
        let mut command_parts = command_text.split('@');
        let command_text = command_parts.next().unwrap();
        if let Some(username) = command_parts.next() {
            if username.to_lowercase() != self.bot_user.username.as_ref().unwrap().to_lowercase() {
                log::debug!("Got command for other bot: {}", username);
                return;
            }
        }

        let args: String = text
            .chars()
            .skip((command.offset + command.length + 1) as usize)
            .collect();

        log::debug!("Got command: {}", command_text);
        log::trace!("Command {} had arguments: {}", command_text, args);

        let from = message.from.clone();

        match command_text {
            "/help" | "/start" => {
                let from = message.from.clone().unwrap();
                let bundle = self.get_fluent_bundle(from.language_code.as_deref());

                use rand::seq::SliceRandom;

                let reply_markup = ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![InlineKeyboardButton {
                        text: "Try Me!".to_string(),
                        switch_inline_query_current_chat: Some(
                            (*STARTING_ARTWORK.choose(&mut rand::thread_rng()).unwrap())
                                .to_string(),
                        ),
                        ..Default::default()
                    }]],
                });

                let send_message = SendMessage {
                    chat_id: message.chat_id(),
                    text: get_message(&bundle, "welcome", None).unwrap(),
                    reply_markup: Some(reply_markup),
                    ..Default::default()
                };

                if let Err(e) = self.bot.make_request(&send_message).await {
                    log::error!("Unable to send help message: {:?}", e);
                    with_user_scope(
                        message.from.as_ref(),
                        Some(vec![("command", command_text.to_string())]),
                        || capture_fail(&e),
                    );
                }
            }
            "/twitter" => self.authenticate_twitter(message).await,
            "/mirror" => self.handle_mirror(message).await,
            "/source" => self.handle_source(message).await,
            _ => log::info!("Unknown command: {}", command_text),
        };

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "command")
            .add_tag("command", command_text)
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = self.influx.query(&point).await {
            log::error!("Unable to send command to InfluxDB: {:?}", e);
            with_user_scope(from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }

    fn send_action(
        &self,
        max: usize,
        chat_id: ChatID,
        user: Option<User>,
        action: ChatAction,
        completed: Arc<AtomicBool>,
    ) {
        use futures_util::stream::StreamExt;
        use std::{sync::atomic::Ordering::SeqCst, time::Duration};

        let bot = self.bot.clone();

        tokio::spawn(async move {
            let chat_action = SendChatAction { chat_id, action };

            let mut count: usize = 0;

            tokio::time::interval(Duration::from_secs(5))
                .take_while(|_| {
                    let completed = completed.load(SeqCst);
                    count += 1;
                    log::trace!(
                        "Evaluating if should send action, completed: {}, count: {}",
                        completed,
                        count
                    );
                    futures::future::ready(!completed && count < max)
                })
                .for_each(|_| async {
                    if let Err(e) = bot.make_request(&chat_action).await {
                        log::warn!("Unable to send chat action: {:?}", e);
                        with_user_scope(user.as_ref(), None, || {
                            capture_fail(&e);
                        });
                    }
                })
                .await;
        });
    }

    async fn handle_source(&mut self, message: Message) {
        let completed = Arc::new(AtomicBool::new(false));
        self.send_action(
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
            completed.clone(),
        );

        let message = if let Some(reply_to_message) = message.reply_to_message {
            *reply_to_message
        } else {
            message
        };

        let photo = match message.photo.clone() {
            Some(photo) if !photo.is_empty() => photo,
            _ => {
                completed.store(true, std::sync::atomic::Ordering::SeqCst);
                self.send_generic_reply(&message, "source-no-photo").await;
                return;
            }
        };

        let biggest = photo
            .iter()
            .max_by_key(|size| size.height * size.width)
            .unwrap();
        let get_file = GetFile {
            file_id: biggest.file_id.to_owned(),
        };

        let file = match self.bot.make_request(&get_file).await {
            Ok(file) => file,
            Err(e) => {
                log::error!("Unable to get file: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "source".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        };

        let bytes = match self.bot.download_file(file.file_path.unwrap()).await {
            Ok(bytes) => bytes,
            Err(e) => {
                log::error!("Unable to download file: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "source".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        };

        let matches = match self.fapi.image_search(bytes, false).await {
            Ok(matches) => matches,
            Err(e) => {
                log::error!("Unable to find matches: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "source".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        };

        let result = match matches.first() {
            Some(result) => result,
            None => {
                self.send_generic_reply(&message, "reverse-no-results")
                    .await;
                return;
            }
        };

        let bundle = self.get_fluent_bundle(message.from.clone().unwrap().language_code.as_deref());

        let name = if result.distance < 5 {
            "reverse-good-result"
        } else {
            "reverse-bad-result"
        };

        let mut args = fluent::FluentArgs::new();
        args.insert("distance", fluent::FluentValue::from(result.distance));
        args.insert(
            "link",
            fluent::FluentValue::from(format!("https://www.furaffinity.net/view/{}/", result.id)),
        );

        let send_message = SendMessage {
            chat_id: message.chat.id.into(),
            text: get_message(&bundle, name, Some(args)).unwrap(),
            disable_web_page_preview: Some(result.distance > 5),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&send_message).await {
            log::error!("Unable to make request: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }

        completed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    async fn send_generic_reply(&mut self, message: &Message, name: &str) {
        let language_code = match &message.from {
            Some(from) => &from.language_code,
            None => return,
        };

        let bundle = self.get_fluent_bundle(language_code.as_deref());
        let send_message = SendMessage {
            chat_id: message.chat_id(),
            reply_to_message_id: Some(message.message_id),
            text: get_message(&bundle, name, None).unwrap(),
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&send_message).await {
            log::error!("Unable to make request: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }

    async fn handle_mirror(&mut self, message: Message) {
        let from = message.from.clone().unwrap();

        let completed = Arc::new(AtomicBool::new(false));
        self.send_action(
            6,
            message.chat_id(),
            message.from.clone(),
            ChatAction::UploadPhoto,
            completed.clone(),
        );

        let message = if let Some(reply_to_message) = message.reply_to_message {
            *reply_to_message
        } else {
            message
        };

        let links: Vec<_> = if let Some(text) = &message.text {
            self.finder.links(&text).collect()
        } else {
            vec![]
        };

        if links.is_empty() {
            self.send_generic_reply(&message, "mirror-no-links").await;
            completed.store(true, std::sync::atomic::Ordering::SeqCst);
            return;
        }

        let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());
        let mut missing: Vec<String> = Vec::new();

        'link: for link in links {
            for site in &mut self.sites {
                let link_str = link.as_str();

                if site.url_supported(link_str).await {
                    log::debug!("Link {} supported by {}", link_str, site.name());

                    let images = match site.get_images(from.id, link_str).await {
                        Ok(images) => images,
                        Err(e) => {
                            missing.push(link_str.to_string());
                            log::warn!("Unable to get image: {:?}", e);
                            with_user_scope(message.from.as_ref(), None, || {
                                capture_fail(&e);
                            });
                            continue 'link;
                        }
                    };

                    let images = match images {
                        Some(images) => images,
                        None => {
                            missing.push(link_str.to_string());
                            continue 'link;
                        }
                    };

                    log::debug!("Found images: {:?}", images);

                    results.extend(images);

                    continue 'link;
                }
            }
        }

        if results.is_empty() {
            self.send_generic_reply(&message, "mirror-no-results").await;
            completed.store(true, std::sync::atomic::Ordering::SeqCst);
            return;
        }

        if results.len() == 1 {
            let result = results.get(0).unwrap();

            let photo = SendPhoto {
                chat_id: message.chat_id(),
                caption: if let Some(source_link) = &result.source_link {
                    Some(source_link.to_owned())
                } else {
                    None
                },
                photo: FileType::URL(result.url.clone()),
                reply_to_message_id: Some(message.message_id),
                ..Default::default()
            };

            completed.store(true, std::sync::atomic::Ordering::SeqCst);

            if let Err(e) = self.bot.make_request(&photo).await {
                log::error!("Unable to make request: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "mirror".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        } else {
            for chunk in results.chunks(10) {
                let media = chunk
                    .iter()
                    .map(|result| {
                        InputMedia::Photo(InputMediaPhoto {
                            media: FileType::URL(result.url.to_owned()),
                            caption: if let Some(source_link) = &result.source_link {
                                Some(source_link.to_owned())
                            } else {
                                None
                            },
                            ..Default::default()
                        })
                    })
                    .collect();

                let media_group = SendMediaGroup {
                    chat_id: message.chat_id(),
                    reply_to_message_id: Some(message.message_id),
                    media,
                    ..Default::default()
                };

                if let Err(e) = self.bot.make_request(&media_group).await {
                    log::error!("Unable to make request: {:?}", e);
                    self.report_error(
                        &message,
                        Some(vec![("command", "mirror".to_string())]),
                        || capture_fail(&e),
                    )
                    .await;
                    return;
                }
            }

            completed.store(true, std::sync::atomic::Ordering::SeqCst);
        }

        if !missing.is_empty() {
            let bundle = self.get_fluent_bundle(from.language_code.as_deref());
            let links: Vec<String> = missing.iter().map(|item| format!("· {}", item)).collect();
            let mut args = fluent::FluentArgs::new();
            args.insert("links", fluent::FluentValue::from(links.join("\n")));

            let send_message = SendMessage {
                chat_id: message.chat_id(),
                reply_to_message_id: Some(message.message_id),
                text: get_message(&bundle, "mirror-missing", Some(args)).unwrap(),
                disable_web_page_preview: Some(true),
                ..Default::default()
            };

            if let Err(e) = self.bot.make_request(&send_message).await {
                log::error!("Unable to make request: {:?}", e);
                with_user_scope(message.from.as_ref(), None, || {
                    capture_fail(&e);
                });
            }
        }
    }

    async fn handle_text(&mut self, message: Message) {
        let now = std::time::Instant::now();

        let text = message.text.clone().unwrap(); // only here because this existed
        let from = message.from.clone().unwrap();

        if text.trim().parse::<i32>().is_err() {
            log::trace!("Got text that wasn't OOB, ignoring");
            return;
        }

        log::trace!("Checking if message was Twitter code");

        let data: (String, String) = match self.db.get(&format!("authenticate:{}", from.id)) {
            Some(data) => data,
            None => return,
        };

        log::trace!("We had waiting Twitter code");

        let request_token = egg_mode::KeyPair::new(data.0, data.1);
        let con_token =
            egg_mode::KeyPair::new(self.consumer_key.clone(), self.consumer_secret.clone());

        let token = match block_on_all(egg_mode::access_token(con_token, &request_token, text)) {
            Err(e) => {
                log::warn!("User was unable to verify OOB: {:?}", e);

                self.report_error(
                    &message,
                    Some(vec![("command", "twitter_auth".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
            Ok(token) => token,
        };

        log::trace!("Got token");

        let access = match token.0 {
            egg_mode::Token::Access { access, .. } => access,
            _ => unimplemented!(),
        };

        log::trace!("Got access token");

        if let Err(e) = self.db.set(
            &format!("credentials:{}", from.id),
            &(access.key, access.secret),
        ) {
            log::warn!("Unable to save user credentials: {:?}", e);

            self.report_error(
                &message,
                Some(vec![("command", "twitter_auth".to_string())]),
                || {
                    sentry::integrations::failure::capture_error(&format_err!(
                        "Unable to save to Twitter database: {}",
                        e
                    ))
                },
            )
            .await;
            return;
        }

        let mut args = fluent::FluentArgs::new();
        args.insert("userName", fluent::FluentValue::from(token.2));

        let bundle = self.get_fluent_bundle(from.language_code.as_deref());

        let message = SendMessage {
            chat_id: from.id.into(),
            text: get_message(&bundle, "twitter-welcome", Some(args)).unwrap(),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&message).await {
            log::warn!("Unable to send message: {:?}", e);
            with_user_scope(Some(&from), None, || {
                capture_fail(&e);
            });
        }

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "added")
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = self.influx.query(&point).await {
            log::error!("Unable to send command to InfluxDB: {:?}", e);
            with_user_scope(Some(&from), None, || {
                capture_fail(&e);
            });
        }
    }

    async fn handle_message(&mut self, update: Update) {
        if let Some(inline) = update.inline_query {
            self.handle_inline(inline).await;
        } else if let Some(message) = update.message {
            if message.photo.is_some() {
                self.process_photo(message).await;
            } else if message.entities.is_some() {
                self.handle_command(message).await;
            } else if message.text.is_some() {
                self.handle_text(message).await;
            }
        } else if let Some(chosen_result) = update.chosen_inline_result {
            let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "chosen")
                .add_field("user_id", chosen_result.from.id);

            if let Err(e) = self.influx.query(&point).await {
                log::error!("Unable to send chosen inline result to InfluxDB: {:?}", e);
                capture_fail(&e);
            }
        }
    }

    async fn authenticate_twitter(&mut self, message: Message) {
        let now = std::time::Instant::now();

        if message.chat.chat_type != "private" {
            self.send_generic_reply(&message, "twitter-private").await;
            return;
        }

        let user = message.from.clone().unwrap();

        let con_token =
            egg_mode::KeyPair::new(self.consumer_key.clone(), self.consumer_secret.clone());

        let request_token = match block_on_all(egg_mode::request_token(&con_token, "oob")) {
            Ok(req) => req,
            Err(e) => {
                log::warn!("Unable to get request token: {:?}", e);

                self.report_error(
                    &message,
                    Some(vec![("command", "twitter".to_string())]),
                    || {
                        sentry::integrations::failure::capture_error(&format_err!(
                            "Unable to get request token: {}",
                            e
                        ))
                    },
                )
                .await;
                return;
            }
        };

        if let Err(e) = self.db.set(
            &format!("authenticate:{}", user.id),
            &(request_token.key.clone(), request_token.secret.clone()),
        ) {
            log::warn!("Unable to save authenticate: {:?}", e);

            self.report_error(
                &message,
                Some(vec![("command", "twitter".to_string())]),
                || {
                    sentry::integrations::failure::capture_error(&format_err!(
                        "Unable to save to Twitter database: {}",
                        e
                    ))
                },
            )
            .await;
            return;
        }

        let url = egg_mode::authorize_url(&request_token);

        let mut args = fluent::FluentArgs::new();
        args.insert("link", fluent::FluentValue::from(url));

        let bundle = self.get_fluent_bundle(user.language_code.as_deref());

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: get_message(&bundle, "twitter-oob", Some(args)).unwrap(),
            reply_markup: Some(ReplyMarkup::ForceReply(ForceReply::selective())),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        if let Err(e) = self.bot.make_request(&send_message).await {
            log::warn!("Unable to send message: {:?}", e);
            with_user_scope(Some(&user), None, || {
                capture_fail(&e);
            });
        }

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "twitter")
            .add_tag("type", "new")
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = self.influx.query(&point).await {
            log::error!("Unable to send command to InfluxDB: {:?}", e);
            with_user_scope(Some(&user), None, || {
                capture_fail(&e);
            });
        }
    }

    fn process_result(&mut self, result: &PostInfo, from: &User) -> Option<Vec<InlineQueryResult>> {
        let bundle = self.get_fluent_bundle(from.language_code.as_deref());

        let mut row = vec![InlineKeyboardButton {
            text: get_message(&bundle, "inline-direct", None).unwrap(),
            url: Some(result.url.clone()),
            callback_data: None,
            ..Default::default()
        }];

        if let Some(source_link) = &result.source_link {
            row.push(InlineKeyboardButton {
                text: get_message(&bundle, "inline-source", None).unwrap(),
                url: Some(source_link.clone()),
                callback_data: None,
                ..Default::default()
            })
        }

        let keyboard = InlineKeyboardMarkup {
            inline_keyboard: vec![row],
        };

        let thumb_url = result.thumb.clone().unwrap_or_else(|| result.url.clone());

        match result.file_type.as_ref() {
            "png" | "jpeg" | "jpg" => {
                let (full_url, thumb_url) = if self.use_proxy {
                    (
                        format!("https://images.weserv.nl/?url={}&output=jpg", result.url),
                        format!(
                            "https://images.weserv.nl/?url={}&output=jpg&w=300",
                            thumb_url
                        ),
                    )
                } else {
                    (result.url.clone(), thumb_url)
                };

                let mut photo = InlineQueryResult::photo(
                    generate_id(),
                    full_url.to_owned(),
                    thumb_url.to_owned(),
                );
                photo.reply_markup = Some(keyboard.clone());

                let mut results = vec![photo];

                if let Some(message) = &result.extra_caption {
                    let mut photo = InlineQueryResult::photo(generate_id(), full_url, thumb_url);
                    photo.reply_markup = Some(keyboard);

                    if let InlineQueryType::Photo(ref mut result) = photo.content {
                        result.caption = Some(message.to_string());
                    }

                    results.push(photo);
                };

                Some(results)
            }
            "gif" => {
                let (full_url, thumb_url) = if self.use_proxy {
                    (
                        format!("https://images.weserv.nl/?url={}&output=gif", result.url),
                        format!(
                            "https://images.weserv.nl/?url={}&output=gif&w=300",
                            thumb_url
                        ),
                    )
                } else {
                    (result.url.clone(), thumb_url)
                };

                let mut gif = InlineQueryResult::gif(
                    generate_id(),
                    full_url.to_owned(),
                    thumb_url.to_owned(),
                );
                gif.reply_markup = Some(keyboard.clone());

                let mut results = vec![gif];

                if let Some(message) = &result.extra_caption {
                    let mut gif = InlineQueryResult::gif(generate_id(), full_url, thumb_url);
                    gif.reply_markup = Some(keyboard);

                    if let InlineQueryType::GIF(ref mut result) = gif.content {
                        result.caption = Some(message.to_string());
                    }

                    results.push(gif);
                };

                Some(results)
            }
            other => {
                log::warn!("Got unusable type: {}", other);
                None
            }
        }
    }

    async fn process_photo(&mut self, message: Message) {
        let now = std::time::Instant::now();

        let completed = Arc::new(AtomicBool::new(false));
        self.send_action(
            12,
            message.chat_id(),
            message.from.clone(),
            ChatAction::Typing,
            completed.clone(),
        );

        let photos = message.photo.clone().unwrap();

        let mut most_pixels = 0;
        let mut file_id = String::default();
        for photo in photos {
            let pixels = photo.height * photo.width;
            if pixels > most_pixels {
                most_pixels = pixels;
                file_id = photo.file_id.clone();
            }
        }

        let get_file = GetFile { file_id };
        let file = match self.bot.make_request(&get_file).await {
            Ok(file) => file,
            Err(e) => {
                log::error!("Unable to get file: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "photo".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        };

        let photo = match self.bot.download_file(file.file_path.unwrap()).await {
            Ok(photo) => photo,
            Err(e) => {
                log::error!("Unable to download file: {:?}", e);
                self.report_error(
                    &message,
                    Some(vec![("command", "photo".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
        };

        let matches = match self.fapi.image_search(photo, false).await {
            Ok(matches) if !matches.is_empty() => matches,
            Err(e) => {
                log::error!("Unable to reverse search image file: {:?}", e);
                completed.store(true, std::sync::atomic::Ordering::SeqCst);
                self.report_error(
                    &message,
                    Some(vec![("command", "photo".to_string())]),
                    || capture_fail(&e),
                )
                .await;
                return;
            }
            _ => {
                let bundle =
                    self.get_fluent_bundle(message.from.clone().unwrap().language_code.as_deref());

                let send_message = SendMessage {
                    chat_id: message.chat_id(),
                    text: get_message(&bundle, "reverse-no-results", None).unwrap(),
                    reply_to_message_id: Some(message.message_id),
                    ..Default::default()
                };

                completed.store(true, std::sync::atomic::Ordering::SeqCst);

                if let Err(e) = self.bot.make_request(&send_message).await {
                    log::error!("Unable to respond to photo: {:?}", e);
                    with_user_scope(message.from.as_ref(), None, || {
                        capture_fail(&e);
                    });
                    self.send_generic_reply(&message, "error-generic").await;
                }

                let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "source")
                    .add_field("matches", 0)
                    .add_field("duration", now.elapsed().as_millis() as i64);

                if let Err(e) = self.influx.query(&point).await {
                    log::error!("Unable to send command to InfluxDB: {:?}", e);
                    with_user_scope(message.from.as_ref(), None, || {
                        capture_fail(&e);
                    });
                }

                return;
            }
        };

        let first = matches.get(0).unwrap();
        log::debug!("Match has distance of {}", first.distance);

        let bundle = self.get_fluent_bundle(message.from.clone().unwrap().language_code.as_deref());

        let name = if first.distance < 5 {
            "reverse-good-result"
        } else {
            "reverse-bad-result"
        };

        let mut args = fluent::FluentArgs::new();
        args.insert("distance", fluent::FluentValue::from(first.distance));
        args.insert(
            "link",
            fluent::FluentValue::from(format!("https://www.furaffinity.net/view/{}/", first.id)),
        );

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: get_message(&bundle, name, Some(args)).unwrap(),
            disable_web_page_preview: Some(first.distance > 5),
            reply_to_message_id: Some(message.message_id),
            ..Default::default()
        };

        completed.store(true, std::sync::atomic::Ordering::SeqCst);

        if let Err(e) = self.bot.make_request(&send_message).await {
            log::error!("Unable to respond to photo: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }

        let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "source")
            .add_tag("good", first.distance < 5)
            .add_field("matches", matches.len() as i64)
            .add_field("duration", now.elapsed().as_millis() as i64);

        if let Err(e) = self.influx.query(&point).await {
            log::error!("Unable to send command to InfluxDB: {:?}", e);
            with_user_scope(message.from.as_ref(), None, || {
                capture_fail(&e);
            });
        }
    }
}
