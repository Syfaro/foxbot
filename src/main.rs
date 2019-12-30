#![feature(try_trait)]

use sites::{PostInfo, Site};
use telegram::*;
use tokio01::runtime::current_thread::block_on_all;

mod sites;

fn generate_id() -> String {
    use rand::Rng;
    let rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .collect()
}

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
    let mut db = PickleDb::load(
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
    let fapi = fautil::FAUtil::new(fa_util_api.clone());

    let mut sites: Vec<Box<dyn Site>> = vec![
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
        Box::new(sites::Direct::new()),
    ];

    let bot =
        Telegram::new(std::env::var("TELEGRAM_APITOKEN").expect("Missing Telegram API token"));

    let mut update_req = GetUpdates::default();
    update_req.timeout = Some(30);

    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);

    loop {
        let updates = match bot.make_request(&update_req).await {
            Ok(updates) => updates,
            Err(e) => {
                log::error!("Unable to get updates: {:?}", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        for update in updates {
            if let Some(inline) = update.inline_query {
                let links: Vec<_> = finder.links(&inline.query).collect();
                let mut results: Vec<PostInfo> = Vec::new();

                log::info!("Got query: {}", inline.query);
                log::debug!("Found links: {:?}", links);

                'link: for link in links {
                    for site in &mut sites {
                        let link_str = link.as_str();

                        if site.is_supported(link_str).await {
                            log::debug!("Link {} supported by {}", link_str, site.name());

                            let images = match site.get_images(inline.from.id, link_str).await {
                                Ok(images) => images,
                                Err(e) => {
                                    log::warn!("Unable to get image: {:?}", e);
                                    continue 'link;
                                }
                            };

                            let images = match images {
                                Some(images) => images,
                                None => continue 'link,
                            };

                            log::debug!("Found images: {:?}", images);

                            results.extend(images);

                            continue 'link;
                        }
                    }
                }

                let mut responses: Vec<InlineQueryResult> = results
                    .iter()
                    .map(process_result)
                    .filter_map(|result| result)
                    .flatten()
                    .collect();

                if responses.is_empty() {
                    responses.push(if inline.query.is_empty() {
                        InlineQueryResult::article(
                            generate_id(),
                            "Type your link or click me for more info".into(),
                            "Hi there! I'm @FoxBot.\n\nBy typing my name into the Telegram message box followed by a link, I'll grab your image and let you send it to your chats while adding a source and direct image link. If you message me directly, you can even add your Twitter account to get content from locked accounts.".into(),
                        )
                    } else {
                        get_empty_query()
                    });
                }

                let answer_inline = AnswerInlineQuery {
                    inline_query_id: inline.id,
                    results: responses,
                    cache_time: None,
                    is_personal: Some(true),
                    next_offset: None,
                    switch_pm_text: None,
                    switch_pm_parameter: None,
                };

                if let Err(e) = bot.make_request(&answer_inline).await {
                    log::error!("Unable to respond to inline: {:?}", e);
                }
            } else if let Some(message) = update.message {
                if message.photo.is_some() {
                    process_photo(&bot, &fapi, message).await;
                } else if let Some(entities) = message.entities {
                    let command = entities
                        .iter()
                        .find(|entity| entity.entity_type == MessageEntityType::BotCommand);
                    let command = match command {
                        Some(command) => command,
                        None => continue,
                    };
                    let text = message.text.unwrap();
                    let command_text: String = text
                        .chars()
                        .skip(command.offset as usize)
                        .take(command.length as usize)
                        .collect();
                    let args: String = text
                        .chars()
                        .skip((command.offset + command.length + 1) as usize)
                        .collect();
                    log::debug!("Got command: {}", command_text);
                    log::trace!("Command {} had arguments: {}", command_text, args);

                    match command_text.as_ref() {
                        "/twitter" => {
                            authenticate_twitter(
                                &mut db,
                                &bot,
                                message.message_id,
                                &message.from.unwrap(),
                            )
                            .await;
                        }
                        _ => log::info!("Unknown command: {}", command_text),
                    };
                } else if let Some(text) = message.text {
                    let from = message.from.unwrap();

                    log::trace!("Checking if message was Twitter code");

                    let data: (String, String) = match db.get(&format!("authenticate:{}", from.id))
                    {
                        Some(data) => data,
                        None => continue,
                    };

                    log::trace!("We had waiting Twitter code");

                    let request_token = egg_mode::KeyPair::new(data.0, data.1);
                    let con_token =
                        egg_mode::KeyPair::new(consumer_key.clone(), consumer_secret.clone());

                    let token =
                        match block_on_all(egg_mode::access_token(con_token, &request_token, text))
                        {
                            Err(e) => {
                                log::warn!("User was unable to verify OOB: {:?}", e);

                                let message = SendMessage {
                                    chat_id: from.id.into(),
                                    text: "Something went wrong, please try again later."
                                        .to_string(),
                                    reply_to_message_id: Some(message.message_id),
                                    ..Default::default()
                                };

                                if let Err(e) = bot.make_request(&message).await {
                                    log::warn!("Unable to send message: {:?}", e);
                                }
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

                    if let Err(e) = db.set(
                        &format!("credentials:{}", from.id),
                        &(access.key, access.secret),
                    ) {
                        log::warn!("Unable to save user credentials: {:?}", e);

                        let message = SendMessage {
                            chat_id: from.id.into(),
                            text: "Something went wrong, please try again later.".to_string(),
                            reply_to_message_id: Some(message.message_id),
                            ..Default::default()
                        };

                        if let Err(e) = bot.make_request(&message).await {
                            log::warn!("Unable to send message: {:?}", e);
                        }
                        return;
                    }

                    let message = SendMessage {
                        chat_id: from.id.into(),
                        text: format!("Welcome aboard, {}!", token.2),
                        reply_to_message_id: Some(message.message_id),
                        ..Default::default()
                    };

                    if let Err(e) = bot.make_request(&message).await {
                        log::warn!("Unable to send message: {:?}", e);
                    }
                }
            }

            update_req.offset = Some(update.update_id + 1);
        }
    }
}

async fn authenticate_twitter(db: &mut pickledb::PickleDb, bot: &Telegram, id: i32, user: &User) {
    let (consumer_key, consumer_secret) = (
        std::env::var("TWITTER_CONSUMER_KEY").expect("Missing Twitter consumer key"),
        std::env::var("TWITTER_CONSUMER_SECRET").expect("Missing Twitter consumer secret"),
    );

    let con_token = egg_mode::KeyPair::new(consumer_key, consumer_secret);

    let request_token = match block_on_all(egg_mode::request_token(&con_token, "oob")) {
        Ok(req) => req,
        Err(e) => {
            log::warn!("Unable to get request token: {:?}", e);

            let message = SendMessage {
                chat_id: user.id.into(),
                text: "Something went wrong, please try again later.".to_string(),
                reply_to_message_id: Some(id),
                ..Default::default()
            };

            if let Err(e) = bot.make_request(&message).await {
                log::warn!("Unable to send message: {:?}", e);
            }
            return;
        }
    };

    if let Err(e) = db.set(
        &format!("authenticate:{}", user.id),
        &(request_token.key.clone(), request_token.secret.clone()),
    ) {
        log::warn!("Unable to save authenticate: {:?}", e);

        let message = SendMessage {
            chat_id: user.id.into(),
            text: "Something went wrong, please try again later.".to_string(),
            reply_to_message_id: Some(id),
            ..Default::default()
        };

        if let Err(e) = bot.make_request(&message).await {
            log::warn!("Unable to send message: {:?}", e);
        }
        return;
    }

    let url = egg_mode::authorize_url(&request_token);

    let message = SendMessage {
        chat_id: user.id.into(),
        text: format!(
            "Please follow the link and enter the 6 digit code returned: {}",
            url
        ),
        reply_markup: Some(ReplyMarkup::ForceReply(ForceReply {
            force_reply: true,
            selective: true,
        })),
        reply_to_message_id: Some(id),
        ..Default::default()
    };

    if let Err(e) = bot.make_request(&message).await {
        log::warn!("Unable to send message: {:?}", e);
    }
}

fn get_empty_query() -> InlineQueryResult {
    InlineQueryResult::article(
        generate_id(),
        "No results found".into(),
        "I could not find any results for the provided query.".into(),
    )
}

fn process_result(result: &PostInfo) -> Option<Vec<InlineQueryResult>> {
    let full_url = match &result.full_url {
        Some(full_url) => full_url,
        None => &result.url,
    };

    let mut row = vec![InlineKeyboardButton {
        text: "Direct Link".into(),
        url: Some(full_url.to_owned()),
        callback_data: None,
    }];

    if full_url != &result.caption {
        row.push(InlineKeyboardButton {
            text: "Source".into(),
            url: Some(result.caption.to_owned()),
            callback_data: None,
        })
    }

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![row],
    };

    match result.file_type.as_ref() {
        "png" | "jpeg" | "jpg" => {
            let mut photo = InlineQueryResult::photo(
                generate_id(),
                result.url.to_owned(),
                result.thumb.to_owned(),
            );
            photo.reply_markup = Some(keyboard.clone());

            let mut results = vec![photo];

            if let Some(message) = &result.message {
                let mut photo = InlineQueryResult::photo(
                    generate_id(),
                    result.url.to_owned(),
                    result.thumb.to_owned(),
                );
                photo.reply_markup = Some(keyboard);

                if let InlineQueryType::Photo(ref mut result) = photo.content {
                    result.caption = Some(message.to_string());
                }

                results.push(photo);
            };

            Some(results)
        }
        "gif" => {
            let mut gif = InlineQueryResult::gif(
                generate_id(),
                result.url.to_owned(),
                result.thumb.to_owned(),
            );
            gif.reply_markup = Some(keyboard.clone());

            let mut results = vec![gif];

            if let Some(message) = &result.message {
                let mut gif = InlineQueryResult::gif(
                    generate_id(),
                    result.url.to_owned(),
                    result.thumb.to_owned(),
                );
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

async fn process_photo(bot: &Telegram, fapi: &fautil::FAUtil, message: Message) {
    let chat_action = SendChatAction {
        chat_id: message.chat.id.into(),
        action: ChatAction::Typing,
    };
    if let Err(e) = bot.make_request(&chat_action).await {
        log::warn!("Unable to send chat action: {:?}", e);
    }

    let photos = message.photo.unwrap();

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
    let file = match bot.make_request(&get_file).await {
        Ok(file) => file,
        _ => return,
    };

    let photo = match bot.download_file(file.file_path.unwrap()).await {
        Ok(photo) => photo,
        _ => return,
    };

    if let Err(e) = bot.make_request(&chat_action).await {
        log::warn!("Unable to send chat action: {:?}", e);
    }

    let matches = match fapi.image_search(photo).await {
        Ok(matches) if !matches.is_empty() => matches,
        _ => {
            let message = SendMessage {
                chat_id: message.chat.id.into(),
                text: "I was unable to find anything, sorry.".to_owned(),
                reply_to_message_id: Some(message.message_id),
                ..Default::default()
            };

            if let Err(e) = bot.make_request(&message).await {
                log::error!("Unable to respond to photo: {:?}", e);
            }

            return;
        }
    };

    let first = matches.get(0).unwrap();
    log::debug!("Match has distance of {}", first.distance);

    let (text, hide_preview) = if first.distance < 5 {
        (
            format!(
                "I found this (distance of {}): https://www.furaffinity.net/view/{}/",
                first.distance, first.id
            ),
            false,
        )
    } else {
        (
            format!("I found this but it may not be the same image, be warned (distance of {}): https://www.furaffinity.net/view/{}/", first.distance, first.id),
            true,
        )
    };

    let message = SendMessage {
        chat_id: message.chat.id.into(),
        text,
        disable_web_page_preview: Some(hide_preview),
        reply_to_message_id: Some(message.message_id),
        ..Default::default()
    };

    if let Err(e) = bot.make_request(&message).await {
        log::error!("Unable to respond to photo: {:?}", e);
    }
}
