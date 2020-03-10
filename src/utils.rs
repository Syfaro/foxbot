use sentry::integrations::failure::capture_fail;
use std::sync::Arc;
use std::time::Instant;
use tracing_futures::Instrument;

use crate::BoxedSite;

type Bundle<'a> = &'a fluent::FluentBundle<fluent::FluentResource>;

pub struct SiteCallback<'a> {
    pub site: &'a BoxedSite,
    pub link: &'a str,
    pub duration: i64,
    pub results: Vec<crate::PostInfo>,
}

#[tracing::instrument(skip(user, sites, callback))]
pub async fn find_images<'a, C>(
    user: &tgbotapi::User,
    links: Vec<&'a str>,
    sites: &mut [BoxedSite],
    callback: &mut C,
) -> failure::Fallible<Vec<&'a str>>
where
    C: FnMut(SiteCallback),
{
    let mut missing = vec![];

    'link: for link in links {
        for site in sites.iter_mut() {
            let start = Instant::now();

            if site.url_supported(link).await {
                tracing::debug!("link {} supported by {}", link, site.name());

                let images = site.get_images(user.id, link).await?;

                match images {
                    Some(results) => {
                        tracing::debug!("found images: {:?}", results);
                        callback(SiteCallback {
                            site: &site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                        continue 'link;
                    }
                    _ => {
                        tracing::debug!("no images found");
                        missing.push(link);
                        continue 'link;
                    }
                }
            }
        }
    }

    Ok(missing)
}

pub fn find_best_photo(sizes: &[tgbotapi::PhotoSize]) -> Option<&tgbotapi::PhotoSize> {
    sizes.iter().max_by_key(|size| size.height * size.width)
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

pub fn with_user_scope<C, R>(from: Option<&tgbotapi::User>, tags: SentryTags, callback: C) -> R
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

type AlternateItems<'a> = Vec<(&'a Vec<String>, &'a Vec<fuzzysearch::File>)>;

pub fn build_alternate_response(bundle: Bundle, mut items: AlternateItems) -> (String, Vec<i64>) {
    let mut used_hashes = vec![];

    items.sort_by(|a, b| {
        let a_distance: u64 = a.1.iter().map(|item| item.distance.unwrap()).sum();
        let b_distance: u64 = b.1.iter().map(|item| item.distance.unwrap()).sum();

        a_distance.partial_cmp(&b_distance).unwrap()
    });

    let mut s = String::new();
    s.push_str(&get_message(&bundle, "alternate-title", None).unwrap());
    s.push_str("\n\n");

    for item in items {
        let total_dist: u64 = item.1.iter().map(|item| item.distance.unwrap()).sum();
        tracing::trace!("total distance: {}", total_dist);
        if total_dist > (6 * item.1.len() as u64) {
            tracing::trace!("too high, aborting");
            continue;
        }
        let artist_name = item
            .1
            .first()
            .unwrap()
            .artists
            .clone()
            .unwrap_or_else(|| vec!["Unknown".to_string()])
            .join(", ");
        let mut args = fluent::FluentArgs::new();
        args.insert("name", artist_name.into());
        s.push_str(&get_message(&bundle, "alternate-posted-by", Some(args)).unwrap());
        s.push_str("\n");
        let mut subs: Vec<fuzzysearch::File> = item.1.to_vec();
        subs.sort_by(|a, b| a.id.partial_cmp(&b.id).unwrap());
        subs.dedup_by(|a, b| a.id == b.id);
        subs.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());
        for sub in subs {
            tracing::trace!("looking at {}-{}", sub.site_name(), sub.id);
            let mut args = fluent::FluentArgs::new();
            args.insert("link", sub.url().into());
            args.insert("distance", sub.distance.unwrap().into());
            s.push_str(&get_message(&bundle, "alternate-distance", Some(args)).unwrap());
            s.push_str("\n");
            used_hashes.push(sub.hash.unwrap());
        }
        s.push_str("\n");
    }

    (s, used_hashes)
}

pub fn parse_known_bots(message: &tgbotapi::Message) -> Option<Vec<String>> {
    let from = if let Some(ref forward_from) = message.forward_from {
        Some(forward_from)
    } else {
        message.from.as_ref()
    };

    let from = match &from {
        Some(from) => from,
        None => return None,
    };

    tracing::trace!("evaluating if known bot: {}", from.id);

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

pub struct ContinuousAction {
    tx: Option<tokio::sync::oneshot::Sender<bool>>,
}

#[tracing::instrument(skip(bot, user))]
pub fn continuous_action(
    bot: Arc<tgbotapi::Telegram>,
    max: usize,
    chat_id: tgbotapi::requests::ChatID,
    user: Option<tgbotapi::User>,
    action: tgbotapi::requests::ChatAction,
) -> ContinuousAction {
    use futures::future::FutureExt;
    use futures_util::stream::StreamExt;
    use std::time::Duration;

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(
        async move {
            let chat_action = tgbotapi::requests::SendChatAction { chat_id, action };

            let mut count: usize = 0;

            let timer = Box::pin(
                tokio::time::interval(Duration::from_secs(5))
                    .take_while(|_| {
                        tracing::trace!(count, "evaluating chat action");
                        count += 1;
                        futures::future::ready(count < max)
                    })
                    .for_each(|_| async {
                        if let Err(e) = bot.make_request(&chat_action).await {
                            tracing::warn!("unable to send chat action: {:?}", e);
                            with_user_scope(user.as_ref(), None, || {
                                capture_fail(&e);
                            });
                        }
                    }),
            )
            .fuse();

            let was_ended = if let futures::future::Either::Right(_) =
                futures::future::select(timer, rx).await
            {
                true
            } else {
                false
            };

            tracing::trace!(count, was_ended, "chat action ended");
        }
        .in_current_span(),
    );

    ContinuousAction { tx: Some(tx) }
}

impl Drop for ContinuousAction {
    fn drop(&mut self) {
        let tx = std::mem::replace(&mut self.tx, None);
        if let Some(tx) = tx {
            tx.send(true).unwrap();
        }
    }
}

#[tracing::instrument(skip(bot, conn, fapi))]
pub async fn match_image(
    bot: &tgbotapi::Telegram,
    conn: &quaint::pooled::Quaint,
    fapi: &fuzzysearch::FuzzySearch,
    file: &tgbotapi::PhotoSize,
) -> failure::Fallible<Vec<fuzzysearch::File>> {
    use quaint::prelude::*;

    let conn = conn.check_out().await?;

    let result = conn
        .select(
            Select::from_table("file_id_cache")
                .column("hash")
                .so_that("file_id".equals(file.file_unique_id.to_string())),
        )
        .await?;

    if let Some(row) = result.first() {
        let hash = row["hash"].as_i64().unwrap();
        return lookup_single_hash(&fapi, hash).await;
    }

    let get_file = tgbotapi::requests::GetFile {
        file_id: file.file_id.clone(),
    };

    let file_info = bot.make_request(&get_file).await?;
    let data = bot.download_file(&file_info.file_path.unwrap()).await?;

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&data)).await??;

    conn.insert(
        Insert::single_into("file_id_cache")
            .value("hash", hash)
            .value("file_id", file.file_unique_id.clone())
            .build(),
    )
    .await?;

    lookup_single_hash(&fapi, hash).await
}

async fn lookup_single_hash(
    fapi: &fuzzysearch::FuzzySearch,
    hash: i64,
) -> failure::Fallible<Vec<fuzzysearch::File>> {
    let mut matches = fapi.lookup_hashes(vec![hash]).await?;

    for mut m in &mut matches {
        m.distance =
            hamming::distance_fast(&m.hash.unwrap().to_be_bytes(), &hash.to_be_bytes()).ok();
    }

    matches.sort_by(|a, b| {
        a.distance
            .unwrap()
            .partial_cmp(&b.distance.unwrap())
            .unwrap()
    });

    Ok(matches)
}
