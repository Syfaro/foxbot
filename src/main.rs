#![feature(try_trait)]

use sites::{PostInfo, Site};
use telegram::*;

mod sites;

fn generate_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .collect()
}

fn main() {
    pretty_env_logger::init();

    let mut sites: Vec<Box<dyn Site>> = vec![
        Box::new(sites::Weasyl::new(std::env::var("WEASYL_APITOKEN").expect("Missing Weasyl API token"))),
        Box::new(sites::Mastodon::new()),
        Box::new(sites::Direct::new()),
    ];

    let bot = Telegram::new(std::env::var("TELEGRAM_APITOKEN").expect("Missing Telegram API token"));

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

                bot.make_request(&answer_inline).unwrap();
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
