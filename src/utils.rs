use failure::ResultExt;
use sentry::integrations::failure::capture_fail;
use std::sync::Arc;
use std::time::Instant;
use tracing_futures::Instrument;

use crate::models::{CachedPost, FileCache, Sites, UserConfig, UserConfigKey};
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

                let images = site
                    .get_images(user.id, link)
                    .await
                    .context("unable to extract site images")?;

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

pub struct ImageInfo {
    url: String,
    dimensions: (u32, u32),
}

#[tracing::instrument(skip(conn, s3, s3_bucket, s3_url))]
async fn upload_image(
    conn: &quaint::pooled::PooledConnection,
    s3: &rusoto_s3::S3Client,
    s3_bucket: &str,
    s3_url: &str,
    url: &str,
    thumb: bool,
) -> failure::Fallible<ImageInfo> {
    if let Some(cached_post) = CachedPost::get(&conn, &url, thumb)
        .await
        .context("unable to get cached post")?
    {
        return Ok(ImageInfo {
            url: cached_post.cdn_url,
            dimensions: cached_post.dimensions,
        });
    }

    let data = reqwest::get(url).await?.bytes().await?;

    let info = infer::Infer::new();

    let im = image::load_from_memory(&data)?;

    let (im, buf) = match info.get(&data) {
        Some(inf) if !thumb && inf.mime == "image/jpeg" => (im, data),
        _ => {
            use bytes::buf::BufMutExt;
            let im = if thumb { im.thumbnail(400, 400) } else { im };
            let mut buf = bytes::BytesMut::with_capacity(2 * 1024 * 1024).writer();
            im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(90))?;
            (im, buf.into_inner().freeze())
        }
    };

    use image::GenericImageView;
    let dimensions = im.dimensions();

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&buf);
    let hash = hasher.finalize();
    let hash = hex::encode(hash);

    let name = if thumb { "thumb" } else { "image" };

    let key = format!("{}/{}/{}_{}.jpg", &hash[0..2], &hash[2..4], name, &hash);

    let put = rusoto_s3::PutObjectRequest {
        acl: Some("public-read".into()),
        bucket: s3_bucket.to_string(),
        content_type: Some("image/jpeg".into()),
        key: key.clone(),
        body: Some(buf.to_vec().into()),
        content_length: Some(buf.len() as i64),
        ..Default::default()
    };

    use rusoto_s3::S3;
    s3.put_object(put).await.unwrap();

    let cdn_url = format!("{}/{}/{}", s3_url, s3_bucket, key);

    if let Err(err) = CachedPost::save(&conn, &url, &cdn_url, thumb, dimensions).await {
        sentry::integrations::failure::capture_error(&err);
    }

    Ok(ImageInfo {
        url: cdn_url,
        dimensions,
    })
}

pub async fn cache_post(
    conn: &quaint::pooled::PooledConnection,
    s3: &rusoto_s3::S3Client,
    s3_bucket: &str,
    s3_url: &str,
    post: &crate::PostInfo,
) -> failure::Fallible<crate::PostInfo> {
    let image = upload_image(&conn, &s3, &s3_bucket, &s3_url, &post.url, false).await?;

    let thumb = if let Some(thumb) = &post.thumb {
        Some(
            upload_image(&conn, &s3, &s3_bucket, &s3_url, &thumb, true)
                .await?
                .url,
        )
    } else {
        None
    };

    let (url, dims) = (image.url, image.dimensions);

    Ok(crate::PostInfo {
        url,
        image_dimensions: Some(dims),
        thumb,
        ..post.to_owned()
    })
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
    let conn = conn
        .check_out()
        .await
        .context("unable to check out database")?;

    if let Some(hash) = FileCache::get(&conn, &file.file_unique_id)
        .await
        .context("unable to query file cache")?
    {
        return lookup_single_hash(&fapi, hash).await;
    }

    let get_file = tgbotapi::requests::GetFile {
        file_id: file.file_id.clone(),
    };

    let file_info = bot
        .make_request(&get_file)
        .await
        .context("unable to request file info from telegram")?;
    let data = bot
        .download_file(&file_info.file_path.unwrap())
        .await
        .context("unable to download file from telegram")?;

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&data))
        .await
        .context("unable to spawn blocking")?
        .context("unable to hash bytes")?;

    FileCache::set(&conn, &file.file_unique_id, hash)
        .await
        .context("unable to set file cache")?;

    lookup_single_hash(&fapi, hash).await
}

async fn lookup_single_hash(
    fapi: &fuzzysearch::FuzzySearch,
    hash: i64,
) -> failure::Fallible<Vec<fuzzysearch::File>> {
    let mut matches = fapi
        .lookup_hashes(vec![hash])
        .await
        .context("unable to lookup hash")?;

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

pub async fn sort_results(
    conn: &quaint::pooled::Quaint,
    user_id: i32,
    results: &mut Vec<fuzzysearch::File>,
) -> failure::Fallible<()> {
    // If we have 1 or fewer items, we don't need to do any sorting.
    if results.len() <= 1 {
        return Ok(());
    }

    let conn = conn
        .check_out()
        .await
        .context("unable to check out database")?;

    let row: Option<Vec<String>> = UserConfig::get(&conn, UserConfigKey::SiteSortOrder, user_id)
        .await
        .context("unable to get user site sort order")?;
    let sites = match row {
        Some(row) => row.iter().map(|item| item.parse().unwrap()).collect(),
        None => Sites::default_order(),
    };

    results.sort_unstable_by(|a, b| {
        let a_dist = a.distance.unwrap();
        let b_dist = b.distance.unwrap();

        if a_dist != b_dist {
            return a_dist.cmp(&b_dist);
        }

        let a_idx = sites
            .iter()
            .position(|s| s.as_str() == a.site_name())
            .unwrap();
        let b_idx = sites
            .iter()
            .position(|s| s.as_str() == b.site_name())
            .unwrap();

        a_idx.cmp(&b_idx)
    });

    Ok(())
}

pub async fn use_source_name(
    conn: &quaint::pooled::Quaint,
    user_id: i32,
) -> failure::Fallible<bool> {
    let conn = conn
        .check_out()
        .await
        .context("unable to check out database")?;
    let row = UserConfig::get(&conn, UserConfigKey::SourceName, user_id)
        .await
        .context("unable to query user source name config")?
        .unwrap_or(false);

    Ok(row)
}
