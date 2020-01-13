use sentry::integrations::failure::capture_fail;
use std::time::Instant;

use crate::BoxedSite;

type Bundle<'a> = &'a fluent::FluentBundle<fluent::FluentResource>;

pub struct SiteCallback<'a> {
    pub site: &'a BoxedSite,
    pub link: &'a str,
    pub duration: i64,
    pub results: Vec<crate::PostInfo>,
}

pub async fn find_images<'a, C>(
    user: &telegram::User,
    links: Vec<&'a str>,
    sites: &mut [BoxedSite],
    callback: &mut C,
) -> Vec<&'a str>
where
    C: FnMut(SiteCallback),
{
    let mut missing = vec![];

    'link: for link in links {
        for site in sites.iter_mut() {
            let start = Instant::now();

            if site.url_supported(link).await {
                log::debug!("Link {} supported by {}", link, site.name());

                match site.get_images(user.id, link).await {
                    // Got results successfully and there were results
                    // Execute callback with site info and results
                    Ok(results) if results.is_some() => {
                        let results = results.unwrap(); // we know this is safe
                        log::debug!("Found images: {:?}", results);
                        callback(SiteCallback {
                            site: &site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                    }
                    // Got results successfully and there were NO results
                    // Continue onto next link
                    Ok(_) => {
                        missing.push(link);
                        continue 'link;
                    }
                    // Got error while processing, report and move onto next link
                    Err(e) => {
                        missing.push(link);
                        log::warn!("Unable to get results: {:?}", e);
                        with_user_scope(
                            Some(user),
                            Some(vec![("site", site.name().to_string())]),
                            || {
                                capture_fail(&e);
                            },
                        );
                        continue 'link;
                    }
                }
            }
        }
    }

    missing
}

pub fn find_best_photo(sizes: &[telegram::PhotoSize]) -> Option<&telegram::PhotoSize> {
    sizes.iter().max_by_key(|size| size.height * size.width)
}

pub async fn download_by_id(
    bot: &telegram::Telegram,
    file_id: &str,
) -> Result<Vec<u8>, telegram::Error> {
    let get_file = telegram::GetFile {
        file_id: file_id.to_string(),
    };
    let file = bot.make_request(&get_file).await?;
    bot.download_file(file.file_path.unwrap()).await
}

pub fn get_message(
    bundle: Bundle,
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

pub fn with_user_scope<C, R>(from: Option<&telegram::User>, tags: SentryTags, callback: C) -> R
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

pub fn hash_image(bytes: &[u8]) -> [u8; 8] {
    let hasher = furaffinity_rs::get_hasher();
    let image = image::load_from_memory(&bytes).unwrap();
    let mut b: [u8; 8] = [0; 8];
    let hash = hasher.hash_image(&image);
    b.copy_from_slice(&hash.as_bytes());
    b
}

pub fn alternate_feedback_keyboard(bundle: Bundle) -> telegram::InlineKeyboardMarkup {
    telegram::InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            telegram::InlineKeyboardButton {
                text: get_message(&bundle, "alternate-feedback-y", None).unwrap(),
                callback_data: Some("alts,y".to_string()),
                ..Default::default()
            },
            telegram::InlineKeyboardButton {
                text: get_message(&bundle, "alternate-feedback-n", None).unwrap(),
                callback_data: Some("alts,n".to_string()),
                ..Default::default()
            },
        ]],
    }
}

type AlternateItems<'a> = Vec<(&'a i32, &'a Vec<fautil::ImageLookup>)>;

pub fn build_alternate_response(bundle: Bundle, mut items: AlternateItems) -> (String, Vec<i64>) {
    let mut used_hashes = vec![];

    items.sort_by(|a, b| {
        let a_distance: u64 = a.1.iter().map(|item| item.distance).sum();
        let b_distance: u64 = b.1.iter().map(|item| item.distance).sum();

        a_distance.partial_cmp(&b_distance).unwrap()
    });

    let mut s = String::new();
    s.push_str(&get_message(&bundle, "alternate-title", None).unwrap());
    s.push_str("\n\n");

    for item in items {
        let total_dist: u64 = item.1.iter().map(|item| item.distance).sum();
        log::trace!("total distance: {}", total_dist);
        if total_dist > (7 * item.1.len() as u64) {
            log::trace!("too high, aborting");
            continue;
        }
        let artist_name = item.1.first().unwrap().artist_name.to_string();
        let mut args = fluent::FluentArgs::new();
        args.insert(
            "name",
            format!(
                "[{}](https://www.furaffinity.net/user/{}/)",
                artist_name,
                artist_name.replace("_", "")
            )
            .into(),
        );
        s.push_str(&get_message(&bundle, "alternate-posted-by", Some(args)).unwrap());
        s.push_str("\n");
        let mut subs: Vec<fautil::ImageLookup> = item.1.to_vec();
        subs.sort_by(|a, b| a.id.partial_cmp(&b.id).unwrap());
        subs.dedup_by(|a, b| a.id == b.id);
        subs.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());
        for sub in subs {
            log::trace!("looking at {}", sub.id);
            let mut args = fluent::FluentArgs::new();
            args.insert(
                "link",
                format!("https://www.furaffinity.net/view/{}/", sub.id).into(),
            );
            args.insert("distance", sub.distance.into());
            s.push_str(&get_message(&bundle, "alternate-distance", Some(args)).unwrap());
            s.push_str("\n");
            used_hashes.push(sub.hash);
        }
        s.push_str("\n");
    }

    s.push_str(&get_message(&bundle, "alternate-feedback", None).unwrap());

    (s, used_hashes)
}

pub fn parse_known_bots(message: &telegram::Message) -> Option<Vec<String>> {
    let from = if let Some(ref forward_from) = message.forward_from {
        Some(forward_from)
    } else {
        message.from.as_ref()
    };

    let from = match &from {
        Some(from) => from,
        None => return None,
    };

    log::trace!("evaluating if known bot: {}", from.id);

    match from.id {
        // FAwatchbot
        190_600_517 => {
            let urls = match message.entities {
                Some(ref entities) => entities.iter().filter_map(|entity| {
                    { entity.url.as_ref().map(|url| url.to_string()) }
                        .filter(|url| url.contains("furaffinity.net/view/"))
                }),
                None => return None,
            };

            Some(urls.collect())
        }
        _ => None,
    }
}
