#![feature(try_trait)]

use sites::{PostInfo, Site};
use telegram::*;

mod sites;

fn generate_id() -> String {
    use rand::Rng;
    let rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .collect()
}

fn main() {
    pretty_env_logger::init();

    let (fa_a, fa_b) = (
        std::env::var("FA_A").expect("Missing FA token a"),
        std::env::var("FA_B").expect("Missing FA token b"),
    );

    let fa_util_api = std::env::var("FAUTIL_APITOKEN").expect("Missing FA Utility API token");
    let fapi = fautil::FAUtil::new(fa_util_api.clone());

    let mut sites: Vec<Box<dyn Site>> = vec![
        Box::new(sites::FurAffinity::new((fa_a, fa_b), fa_util_api)),
        Box::new(sites::Weasyl::new(
            std::env::var("WEASYL_APITOKEN").expect("Missing Weasyl API token"),
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
        let updates = match bot.make_request(&update_req) {
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

                        if site.is_supported(link_str) {
                            log::debug!("Link {} supported by {}", link_str, site.name());

                            let images = match site.get_images(link_str) {
                                Ok(images) => images,
                                Err(_) => continue 'link,
                            };

                            let images = match images {
                                Some(images) => images,
                                None => continue 'link,
                            };

                            results.extend(images);

                            continue 'link;
                        }
                    }
                }

                let mut responses: Vec<InlineQueryResult> = results
                    .iter()
                    .map(process_result)
                    .filter(Option::is_some)
                    .map(std::option::Option::unwrap)
                    .flatten()
                    .collect();

                if responses.is_empty() {
                    responses.push(get_empty_query());
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

                if let Err(e) = bot.make_request(&answer_inline) {
                    log::error!("Unable to respond to inline: {:?}", e);
                }
            } else if let Some(message) = update.message {
                if message.photo.is_some() {
                    process_photo(&bot, &fapi, message);
                }
            }

            update_req.offset = Some(update.update_id + 1);
        }
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
        _ => None,
    }
}

fn process_photo(bot: &Telegram, fapi: &fautil::FAUtil, message: Message) {
    let chat_action = SendChatAction {
        chat_id: message.chat.id.into(),
        action: ChatAction::Typing,
    };
    let _ = bot.make_request(&chat_action);

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
    let file = match bot.make_request(&get_file) {
        Ok(file) => file,
        _ => return,
    };

    let photo = match bot.download_file(file.file_path) {
        Ok(photo) => photo,
        _ => return,
    };

    let matches = match fapi.image_search(photo) {
        Ok(matches) if !matches.is_empty() => matches,
        _ => return,
    };

    let first = matches.get(0).unwrap();

    let message = SendMessage {
        chat_id: message.chat.id.into(),
        text: format!(
            "I found this: https://www.furaffinity.net/view/{}/",
            first.id
        ),
    };

    if let Err(e) = bot.make_request(&message) {
        log::error!("Unable to respond to photo: {:?}", e);
    }
}
