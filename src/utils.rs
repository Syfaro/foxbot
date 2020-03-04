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
                tracing::debug!("link {} supported by {}", link, site.name());

                match site.get_images(user.id, link).await {
                    // Got results successfully and there were results
                    // Execute callback with site info and results
                    Ok(results) if results.is_some() => {
                        let results = results.unwrap(); // we know this is safe
                        tracing::debug!("found images: {:?}", results);
                        callback(SiteCallback {
                            site: &site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                        break 'link;
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
                        tracing::warn!("unable to get results: {:?}", e);
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

#[tracing::instrument(skip(bot))]
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

type AlternateItems<'a> = Vec<(&'a Vec<String>, &'a Vec<fautil::File>)>;

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
        if total_dist > (7 * item.1.len() as u64) {
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
        let mut subs: Vec<fautil::File> = item.1.to_vec();
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
    bot: Arc<telegram::Telegram>,
    max: usize,
    chat_id: telegram::ChatID,
    user: Option<telegram::User>,
    action: telegram::ChatAction,
) -> ContinuousAction {
    use futures::future::FutureExt;
    use futures_util::stream::StreamExt;
    use std::time::Duration;

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(
        async move {
            let chat_action = telegram::SendChatAction { chat_id, action };

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
