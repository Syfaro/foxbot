use anyhow::Context;
use fuzzysearch::SiteInfo;
use std::time::Instant;
use std::{collections::HashSet, str::FromStr, sync::Arc};
use tgbotapi::FileType;
use tracing_futures::Instrument;

use foxbot_models::{CachedPost, FileCache, Sites, UserConfig, UserConfigKey};
use foxbot_sites::{BoxedSite, PostInfo};

/// Generates a random 24 character alphanumeric string.
///
/// Not cryptographically secure but unique enough for Telegram's unique IDs.
pub fn generate_id() -> String {
    use rand::Rng;
    let rng = rand::thread_rng();

    rng.sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

/// A localization bundle.
type Bundle<'a> = &'a fluent::concurrent::FluentBundle<fluent::FluentResource>;

/// A convenience macro for handlers to ignore updates that don't contain a
/// required field.
#[macro_export]
macro_rules! needs_field {
    ($message:expr, $field:tt) => {
        match $message.$field {
            Some(ref field) => field,
            _ => return Ok(crate::handlers::Status::Ignored),
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

/// Data obtained from a site loader on a given URL.
pub struct SiteCallback<'a> {
    /// The site loader that was used to check for images.
    pub site: &'a BoxedSite,
    /// The link that was loaded.
    pub link: &'a str,
    /// The amount of time it took for the site to load data from the URL.
    pub duration: i64,
    /// The results obtained by the loader.
    pub results: Vec<PostInfo>,
}

/// Find images from the given URLs using the site loaders with authentication
/// from the given user.
///
/// It goes through each URL with each site, in the provided order. When a site
/// specifies that it supports a URL, the images are attempted to be loaded. If
/// it was possible to get images, the callback is called with the data.
/// Otherwise, the URL is added to a list of URLs where no images were found.
/// After a site reports it supports a URL, no other sites are attempted for
/// that URL. When complete, it returns the URLs that appeared to contain no
/// content.
#[tracing::instrument(err, skip(user, sites, callback))]
pub async fn find_images<'a, C>(
    user: &tgbotapi::User,
    links: Vec<&'a str>,
    sites: &mut [BoxedSite],
    callback: &mut C,
) -> anyhow::Result<Vec<&'a str>>
where
    C: FnMut(SiteCallback),
{
    let mut missing = vec![];

    'link: for link in links {
        for site in sites.iter_mut() {
            let start = Instant::now();

            if site.url_supported(link).await {
                tracing::debug!(link, site = site.name(), "found supported link");

                let images = site.get_images(user.id, link).await?;

                match images {
                    Some(results) => {
                        tracing::debug!(site = site.name(), "found images: {:?}", results);
                        callback(SiteCallback {
                            site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                    }
                    _ => {
                        tracing::debug!(site = site.name(), "no images found");
                        missing.push(link);
                    }
                }

                continue 'link;
            }
        }
    }

    Ok(missing)
}

/// Information about an image uploaded to the bot's cache.
pub struct ImageInfo {
    /// URL to the bot's image
    url: String,
    /// Dimensions of the image
    dimensions: (u32, u32),
}

/// Processes an image for inline query results.
///
/// * Checks if URL already exists in cache, if so, returns that
/// * Downloads image
/// * Converts image to JPEG if not already and resizes if thumbnail
/// * Uploads to S3 bucket
/// * Saves in cache
#[tracing::instrument(err, skip(conn, s3, s3_bucket, s3_url, data))]
async fn upload_image(
    conn: &sqlx::Pool<sqlx::Postgres>,
    s3: &rusoto_s3::S3Client,
    s3_bucket: &str,
    s3_url: &str,
    url: &str,
    thumb: bool,
    data: &bytes::Bytes,
) -> anyhow::Result<ImageInfo> {
    use bytes::BufMut;

    if let Some(cached_post) = CachedPost::get(conn, url, thumb)
        .await
        .context("unable to get cached post")?
    {
        return Ok(ImageInfo {
            url: cached_post.cdn_url,
            dimensions: cached_post.dimensions,
        });
    }

    let im = image::load_from_memory(data)?;

    // We need to determine what processing to do, if any, on the image before
    // caching it. We can start by checking if this is a thumbnail. If so, we
    // should thumbnail it to a 400x400 image. Then, check if the image is
    // larger than 5MB. If it is, we should resize it down to 2000x2000.
    // Otherwise, perform no processing on the image and cache the original.
    //
    // This used to always convert images to JPEGs, but it does not appear that
    // any Telegram client actually requires this.
    //
    // When we do have to convert images to JPEGs, make sure to convert them to
    // Rgb8 before attempting to encode the data. Certain images can have larger
    // bit depths that can't be represented as JPEGs and generate an error
    // instead of working as expected.
    let (im, buf) = if thumb {
        let im = im.thumbnail(400, 400);
        let im = image::DynamicImage::ImageRgb8(im.into_rgb8());
        let mut buf = bytes::BytesMut::with_capacity(2_000_000).writer();
        im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(90))?;
        (im, buf.into_inner().freeze())
    } else if data.len() > 5_000_000 {
        let im = im.resize(2000, 2000, image::imageops::FilterType::Lanczos3);
        let im = image::DynamicImage::ImageRgb8(im.into_rgb8());
        let mut buf = bytes::BytesMut::with_capacity(2_000_000).writer();
        im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(90))?;
        (im, buf.into_inner().freeze())
    } else {
        (im, data.clone())
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
    s3.put_object(put).await?;

    let cdn_url = format!("{}/{}/{}", s3_url, s3_bucket, key);

    if let Err(err) = CachedPost::save(conn, url, &cdn_url, thumb, dimensions).await {
        sentry::integrations::anyhow::capture_anyhow(&err);
    }

    Ok(ImageInfo {
        url: cdn_url,
        dimensions,
    })
}

/// Download image from URL and return bytes.
///
/// Will fail if the download is larger than 50MB.
#[tracing::instrument]
pub async fn download_image(url: &str) -> anyhow::Result<bytes::Bytes> {
    let size_check = CheckFileSize::new(url, 50_000_000);
    size_check.into_bytes().await
}

/// Calculate image dimensions from provided data, returning a new PostInfo.
#[tracing::instrument(skip(data))]
pub async fn size_post(post: &PostInfo, data: &bytes::Bytes) -> anyhow::Result<PostInfo> {
    use image::GenericImageView;

    // If we already have image dimensions, assume they're valid and reuse.
    if post.image_dimensions.is_some() {
        return Ok(post.to_owned());
    }

    let im = image::load_from_memory(data)?;
    let dimensions = im.dimensions();

    Ok(PostInfo {
        image_dimensions: Some(dimensions),
        image_size: Some(data.len()),
        ..post.to_owned()
    })
}

/// Download URL from post, calculate image dimensions, convert to JPEG and
/// generate thumbnail, and upload to S3 bucket. Returns a new PostInfo with
/// the updated URLs and dimensions.
#[tracing::instrument(err, skip(conn, s3, s3_bucket, s3_url, data, post), fields(post_url = %post.url))]
pub async fn cache_post(
    conn: &sqlx::Pool<sqlx::Postgres>,
    s3: &rusoto_s3::S3Client,
    s3_bucket: &str,
    s3_url: &str,
    post: &PostInfo,
    data: &bytes::Bytes,
) -> anyhow::Result<PostInfo> {
    let image = upload_image(conn, s3, s3_bucket, s3_url, &post.url, false, data).await?;

    let thumb = if let Some(thumb) = &post.thumb {
        Some(
            upload_image(conn, s3, s3_bucket, s3_url, thumb, true, data)
                .await?
                .url,
        )
    } else {
        None
    };

    let (url, dims) = (image.url, image.dimensions);

    Ok(PostInfo {
        url,
        image_dimensions: Some(dims),
        thumb,
        ..post.to_owned()
    })
}

/// Find the photo with the largest number of pixels.
pub fn find_best_photo(sizes: &[tgbotapi::PhotoSize]) -> Option<&tgbotapi::PhotoSize> {
    sizes.iter().max_by_key(|size| size.height * size.width)
}

/// Get a message from the bundle with a language code, if provided.
pub fn get_message(
    bundle: Bundle,
    name: &str,
    args: Option<fluent::FluentArgs>,
) -> Result<String, Vec<fluent::FluentError>> {
    let msg = bundle
        .get_message(name)
        .with_context(|| format!("attempting to get message bundle: {}", name))
        .expect("message doesn't exist");
    let pattern = msg.value.expect("message has no value");
    let mut errors = vec![];
    let value = bundle.format_pattern(pattern, args.as_ref(), &mut errors);
    if errors.is_empty() {
        Ok(value.to_string())
    } else {
        Err(errors)
    }
}

/// Add current opentelemetry span to a Sentry scope.
pub fn add_sentry_tracing(scope: &mut sentry::Scope) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let context = tracing::Span::current().context();

    let mut headers: reqwest::header::HeaderMap = Default::default();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(
            &context,
            &mut opentelemetry_http::HeaderInjector(&mut headers),
        )
    });

    for (name, value) in headers {
        let name = match name {
            Some(name) => name,
            None => continue,
        };

        let value = match value.to_str() {
            Ok(value) => value,
            Err(_err) => continue,
        };

        scope.set_extra(&name.to_string(), value.into());
    }
}

/// Tags to add to a sentry event.
type SentryTags<'a> = Option<Vec<(&'a str, String)>>;

/// Run a callback within a Sentry scope with a given user and tags.
pub fn with_user_scope<C, R>(
    from: Option<&tgbotapi::User>,
    err: &anyhow::Error,
    tags: SentryTags,
    callback: C,
) -> R
where
    C: FnOnce(&anyhow::Error) -> R,
{
    tracing::trace!(?tags, ?from, "updating sentry scope");

    sentry::with_scope(
        |mut scope| {
            add_sentry_tracing(&mut scope);

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
        || callback(err),
    )
}

/// Possible alternate items.
type AlternateItems<'a> = Vec<(&'a Vec<String>, &'a Vec<fuzzysearch::File>)>;

/// Determine which matches are potential alternate versions of an image.
///
/// It remembers which hashes were associated with used items and returns them
/// in addition to the formatted message.
pub fn build_alternate_response(bundle: Bundle, mut items: AlternateItems) -> (String, Vec<i64>) {
    let mut used_hashes = vec![];

    items.sort_by(|a, b| {
        let a_distance: u64 = a.1.iter().map(|item| item.distance.unwrap()).sum();
        let b_distance: u64 = b.1.iter().map(|item| item.distance.unwrap()).sum();

        a_distance.partial_cmp(&b_distance).unwrap()
    });

    let mut s = String::new();
    s.push_str(&get_message(bundle, "alternate-title", None).unwrap());
    s.push_str("\n\n");

    for item in items {
        let total_dist: u64 = item.1.iter().map(|item| item.distance.unwrap()).sum();
        tracing::trace!(distance = total_dist, "discovered total distance");
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
        s.push_str(&get_message(bundle, "alternate-posted-by", Some(args)).unwrap());
        s.push('\n');
        let mut subs: Vec<fuzzysearch::File> = item.1.to_vec();
        subs.sort_by(|a, b| a.id().partial_cmp(&b.id()).unwrap());
        subs.dedup_by(|a, b| a.id() == b.id());
        subs.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());
        for sub in subs {
            tracing::trace!(site = sub.site_name(), id = %sub.id(), "looking at submission");
            let mut args = fluent::FluentArgs::new();
            args.insert("link", sub.url().into());
            args.insert("distance", sub.distance.unwrap_or(10).into());
            let message = if let Some(rating) = get_rating_bundle_name(&sub.rating) {
                let rating = get_message(bundle, rating, None).unwrap();
                args.insert("rating", rating.into());
                get_message(bundle, "alternate-distance", Some(args)).unwrap()
            } else {
                get_message(bundle, "alternate-distance-unknown", Some(args)).unwrap()
            };
            s.push_str(&message);
            s.push('\n');
            used_hashes.push(sub.hash.unwrap());
        }
        s.push('\n');
    }

    (s, used_hashes)
}

/// An action that is repeatedly sent to Telegram until a maximum number has
/// been reached or it has been dropped.
pub struct ContinuousAction {
    tx: Option<tokio::sync::oneshot::Sender<bool>>,
}

/// Send an action into a chat until the returned value is dropped or the max
/// has been reached.
#[tracing::instrument(skip(bot, user))]
#[must_use]
pub fn continuous_action(
    bot: Arc<tgbotapi::Telegram>,
    max: usize,
    chat_id: tgbotapi::requests::ChatID,
    user: Option<tgbotapi::User>,
    action: tgbotapi::requests::ChatAction,
) -> ContinuousAction {
    use futures::StreamExt;
    use std::time::Duration;
    use tokio_stream::wrappers::IntervalStream;

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(
        async move {
            let chat_action = tgbotapi::requests::SendChatAction { chat_id, action };

            let mut count: usize = 0;

            // Take a new value every 5 seconds until the count has exceeded the
            // max.
            let timer = Box::pin(
                IntervalStream::new(tokio::time::interval(Duration::from_secs(5)))
                    .take_while(|_| {
                        tracing::trace!(count, "evaluating chat action");
                        count += 1;
                        futures::future::ready(count < max)
                    })
                    .for_each(|_| async {
                        if let Err(e) = bot.make_request(&chat_action).await {
                            tracing::warn!("unable to send chat action: {:?}", e);
                            with_user_scope(user.as_ref(), &e.into(), None, |err| {
                                sentry::integrations::anyhow::capture_anyhow(err);
                            });
                        }
                    }),
            );

            // Wait until the value has been dropped (got something on rx) or
            // the max count has been reached.
            let was_ended = matches!(
                futures::future::select(timer, rx).await,
                futures::future::Either::Right(_)
            );

            tracing::trace!(count, was_ended, "chat action ended");
        }
        .in_current_span(),
    );

    ContinuousAction { tx: Some(tx) }
}

// When dropped, take oneshot and send event signaling it's done.
impl Drop for ContinuousAction {
    fn drop(&mut self) {
        let tx = std::mem::take(&mut self.tx);
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
    }
}

/// Attempt to match an image against FuzzySearch by:
/// * Checking if the file ID already exists in the cache
/// * If not, downloading the image and hashing it
/// * Looking up the hash with [`lookup_single_hash`]
#[tracing::instrument(err, skip(bot, redis, fapi))]
pub async fn match_image(
    bot: &tgbotapi::Telegram,
    redis: &redis::aio::ConnectionManager,
    fapi: &fuzzysearch::FuzzySearch,
    file: &tgbotapi::PhotoSize,
    distance: Option<i64>,
) -> anyhow::Result<(i64, Vec<fuzzysearch::File>)> {
    if let Some(hash) = FileCache::get(redis, &file.file_unique_id)
        .await
        .context("unable to query file cache")?
    {
        return lookup_single_hash(fapi, hash, distance)
            .await
            .map(|files| (hash, files));
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
        .instrument(tracing::debug_span!("hash_bytes"))
        .await
        .context("unable to spawn blocking")?
        .context("unable to hash bytes")?;

    FileCache::set(redis, &file.file_unique_id, hash)
        .await
        .context("unable to set file cache")?;

    lookup_single_hash(fapi, hash, distance)
        .await
        .map(|files| (hash, files))
}

/// Lookup a single hash from FuzzySearch, ensuring that the distance has been
/// calculated from the provided hash.
pub async fn lookup_single_hash(
    fapi: &fuzzysearch::FuzzySearch,
    hash: i64,
    distance: Option<i64>,
) -> anyhow::Result<Vec<fuzzysearch::File>> {
    let mut matches = fapi
        .lookup_hashes(&[hash], distance)
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

    // Twitter general rating is probably bad, remove it.
    matches.iter_mut().for_each(|m| {
        if !matches!(m.site_info, Some(SiteInfo::Twitter)) {
            return;
        }

        if matches!(m.rating, Some(fuzzysearch::Rating::General)) {
            m.rating = None;
        }
    });

    Ok(matches)
}

/// Sort match results based on a user's preferences.
pub async fn sort_results(
    conn: &sqlx::Pool<sqlx::Postgres>,
    user_id: i64,
    results: &mut Vec<fuzzysearch::File>,
) -> anyhow::Result<()> {
    // If we have 1 or fewer items, we don't need to do any sorting.
    if results.len() <= 1 {
        return Ok(());
    }

    let row: Option<Vec<String>> = UserConfig::get(conn, UserConfigKey::SiteSortOrder, user_id)
        .await
        .context("unable to get user site sort order")?;
    let sites = match row {
        Some(row) => row.iter().map(|item| item.parse().unwrap()).collect(),
        None => Sites::default_order(),
    };

    sort_results_by(&sites, results, false);

    Ok(())
}

/// Sort match results with a given order.
///
/// This expects that undesired results have already been filtered.
///
/// If `site_first` is true, results will be sorted by site order preference
/// then by distance. If it is false, results will be sorted by distance then
/// site order.
pub fn sort_results_by(order: &[Sites], results: &mut [fuzzysearch::File], site_first: bool) {
    results.sort_unstable_by(|a, b| {
        let a_dist = a.distance.unwrap();
        let b_dist = b.distance.unwrap();

        let a_idx = order
            .iter()
            .position(|s| s.as_str() == a.site_name())
            .unwrap_or_default();
        let b_idx = order
            .iter()
            .position(|s| s.as_str() == b.site_name())
            .unwrap_or_default();

        if !site_first && a_dist != b_dist {
            return a_dist.cmp(&b_dist);
        } else if site_first && a_idx != b_idx {
            return a_idx.cmp(&b_idx);
        }

        if !site_first {
            a_idx.cmp(&b_idx)
        } else {
            a_dist.cmp(&b_dist)
        }
    });
}

/// Check if any of the provided image URLs have a hash similar to the given
/// input.
#[tracing::instrument(skip(urls))]
pub async fn has_similar_hash(to: i64, urls: &[&str]) -> bool {
    let to = to.to_be_bytes();

    for url in urls {
        let check_size = CheckFileSize::new(url, 50_000_000);
        let bytes = match check_size.into_bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("unable to download image: {:?}", err);

                continue;
            }
        };

        let hash = tokio::task::spawn_blocking(move || {
            use std::convert::TryInto;

            let hasher = fuzzysearch::get_hasher();

            let im = match image::load_from_memory(&bytes) {
                Ok(im) => im,
                Err(err) => {
                    tracing::warn!("unable to load image: {:?}", err);

                    return None;
                }
            };

            let hash = hasher.hash_image(&im);
            let bytes: [u8; 8] = hash.as_bytes().try_into().unwrap_or_default();

            Some(bytes)
        })
        .in_current_span()
        .await
        .unwrap_or_default();

        let hash = match hash {
            Some(hash) => hash,
            _ => continue,
        };

        if hamming::distance_fast(&to, &hash).unwrap() <= 3 {
            tracing::debug!(url, hash = i64::from_be_bytes(hash), "hashes were similar");

            return true;
        }
    }

    false
}

/// Get the first match for each site.
///
/// This expects that the results have already been sorted based on distance and
/// filtered for undesired results.
pub fn first_of_each_site(results: &[fuzzysearch::File]) -> Vec<(Sites, fuzzysearch::File)> {
    let mut firsts = Vec::with_capacity(Sites::default_order().len());
    let mut seen = HashSet::new();

    for result in results {
        let site = match Sites::from_str(result.site_name()) {
            Ok(site) => site,
            _ => continue,
        };

        if seen.contains(&site) {
            continue;
        }

        seen.insert(site.clone());
        firsts.push((site, result.to_owned()));
    }

    firsts
}

/// Extract all possible links from a Message. It looks at the text,
/// caption, and all buttons within an inline keyboard. Uses URL parsing from
/// Telegram.
pub fn extract_links(message: &tgbotapi::Message) -> Vec<&str> {
    let mut links: Vec<&str> = vec![];

    // See if it was posted with a bot that included an inline keyboard.
    if let Some(ref markup) = message.reply_markup {
        for row in &markup.inline_keyboard {
            for button in row {
                if let Some(url) = &button.url {
                    links.push(url);
                }
            }
        }
    }

    if let Some(ref entities) = message.entities {
        links.extend(extract_entity_links(
            message.text.as_ref().unwrap(),
            entities,
        ));
    }

    if let Some(ref entities) = message.caption_entities {
        links.extend(extract_entity_links(
            message.caption.as_ref().unwrap(),
            entities,
        ));
    }

    links
}

/// Process all entities in Telegram message to find links.
fn extract_entity_links<'a>(
    text: &'a str,
    entities: &'a [tgbotapi::MessageEntity],
) -> Vec<&'a str> {
    let mut links: Vec<&str> = vec![];

    for entity in entities {
        if entity.entity_type == tgbotapi::MessageEntityType::TextLink {
            links.push(entity.url.as_ref().unwrap());
        } else if entity.entity_type == tgbotapi::MessageEntityType::Url {
            links.push(get_entity_text(text, entity));
        }
    }

    links
}

/// Try to calculate a UTF16 position in a UTF8 string.
///
/// This is required because Telegram provides entities in UTF16 code units.
///
/// # Panics
///
/// This will panic if the position is not available in the data.
fn utf16_pos_in_utf8(data: &str, pos: usize) -> usize {
    let mut utf8_pos: usize = 0;
    let mut utf16_pos: usize = 0;

    for c in data.chars() {
        if utf16_pos == pos {
            return utf8_pos;
        }

        utf8_pos += c.len_utf8();
        utf16_pos += c.len_utf16();
    }

    if utf16_pos == pos {
        return utf8_pos;
    }

    panic!("invalid utf-16 position");
}

/// Extract text from a message entity.
fn get_entity_text<'a>(text: &'a str, entity: &tgbotapi::MessageEntity) -> &'a str {
    let start = utf16_pos_in_utf8(text, entity.offset as usize);
    let end = utf16_pos_in_utf8(text, (entity.offset + entity.length) as usize);

    &text[start..end]
}

/// Check if a link was contained within a linkify Link.
pub fn link_was_seen(
    sites: &tokio::sync::MutexGuard<Vec<BoxedSite>>,
    links: &[&str],
    source: &str,
) -> bool {
    // Find the unique ID for the source link. If one does not exist, we can't
    // find any matches against it.
    let source_id = match sites.iter().find_map(|site| site.url_id(source)) {
        Some(source) => source,
        _ => return false,
    };

    links.iter().any(
        |link| match sites.iter().find_map(|site| site.url_id(link)) {
            Some(link_id) => link_id == source_id,
            _ => false,
        },
    )
}

/// Find the user responsible for any type of update.
pub fn user_from_update(update: &tgbotapi::Update) -> Option<&tgbotapi::User> {
    use tgbotapi::*;

    match &update {
        Update {
            message: Some(Message { from, .. }),
            ..
        } => from.as_ref(),
        Update {
            edited_message: Some(Message { from, .. }),
            ..
        } => from.as_ref(),
        Update {
            channel_post: Some(Message { from, .. }),
            ..
        } => from.as_ref(),
        Update {
            edited_channel_post: Some(Message { from, .. }),
            ..
        } => from.as_ref(),
        Update {
            inline_query: Some(InlineQuery { from, .. }),
            ..
        } => Some(from),
        Update {
            callback_query: Some(CallbackQuery { from, .. }),
            ..
        } => Some(from),
        _ => None,
    }
}

/// Find the chat responsible for any type of update.
pub fn chat_from_update(update: &tgbotapi::Update) -> Option<&tgbotapi::Chat> {
    use tgbotapi::*;

    let chat = match &update {
        Update {
            message: Some(message),
            ..
        } => &message.chat,
        Update {
            edited_message: Some(message),
            ..
        } => &message.chat,
        Update {
            channel_post: Some(message),
            ..
        } => &message.chat,
        Update {
            edited_channel_post: Some(message),
            ..
        } => &message.chat,
        Update {
            my_chat_member: Some(chat_member),
            ..
        } => &chat_member.chat,
        Update {
            chat_member: Some(chat_member),
            ..
        } => &chat_member.chat,
        _ => return None,
    };

    Some(chat)
}

/// Check if the bot has permissions to delete in a chat. Checks the cache if
/// not being told to ignore it.
pub async fn can_delete_in_chat(
    bot: &tgbotapi::Telegram,
    conn: &sqlx::Pool<sqlx::Postgres>,
    chat_id: i64,
    user_id: i64,
    ignore_cache: bool,
) -> anyhow::Result<bool> {
    use foxbot_models::{GroupConfig, GroupConfigKey};

    // If we're not ignoring cache, start by checking if we already have a value
    // in the database.
    if !ignore_cache {
        let can_delete: Option<bool> =
            GroupConfig::get(conn, chat_id, GroupConfigKey::HasDeletePermission).await?;

        // If we had a value we're done, return it.
        if let Some(can_delete) = can_delete {
            return Ok(can_delete);
        }
    }

    // Load the user ID (likely the Bot's user ID), check if they have the
    // permission to delete messages.
    let chat_member = bot
        .make_request(&tgbotapi::requests::GetChatMember {
            chat_id: chat_id.into(),
            user_id,
        })
        .await?;
    let can_delete = chat_member.can_delete_messages.unwrap_or(false);

    // Cache the new value.
    GroupConfig::set(
        conn,
        GroupConfigKey::HasDeletePermission,
        chat_id,
        can_delete,
    )
    .await?;

    Ok(can_delete)
}

/// Get the name of the localization for a given rating, or if it's unknown.
pub fn get_rating_bundle_name(rating: &Option<fuzzysearch::Rating>) -> Option<&'static str> {
    match rating {
        Some(fuzzysearch::Rating::General) => Some("rating-general"),
        Some(fuzzysearch::Rating::Mature) | Some(fuzzysearch::Rating::Adult) => {
            Some("rating-adult")
        }
        None => None,
    }
}

/// Write a reply for matched sources.
pub fn source_reply(matches: &[fuzzysearch::File], bundle: Bundle<'_>) -> String {
    let first = match matches.first() {
        Some(result) => result,
        None => return get_message(bundle, "reverse-no-results", None).unwrap(),
    };

    let similar: Vec<&fuzzysearch::File> = matches
        .iter()
        .skip(1)
        .take_while(|m| m.distance.unwrap() == first.distance.unwrap())
        .collect();
    tracing::debug!(
        distance = first.distance.unwrap(),
        "discovered match distance"
    );

    if similar.is_empty() {
        let mut args = fluent::FluentArgs::new();
        args.insert("link", first.url().into());

        if let Some(rating) = get_rating_bundle_name(&first.rating) {
            let rating = get_message(bundle, rating, None).unwrap();
            args.insert("rating", rating.into());

            get_message(bundle, "reverse-result", Some(args)).unwrap()
        } else {
            get_message(bundle, "reverse-result-unknown", Some(args)).unwrap()
        }
    } else {
        let mut items = Vec::with_capacity(2 + similar.len());

        let text = get_message(bundle, "reverse-multiple-results", None).unwrap();
        items.push(text);

        for file in vec![first].into_iter().chain(similar) {
            let mut args = fluent::FluentArgs::new();
            args.insert("link", file.url().into());

            let result = if let Some(rating) = get_rating_bundle_name(&file.rating) {
                let rating = get_message(bundle, rating, None).unwrap();
                args.insert("rating", rating.into());

                get_message(bundle, "reverse-multiple-item", Some(args)).unwrap()
            } else {
                get_message(bundle, "reverse-multiple-item-unknown", Some(args)).unwrap()
            };

            items.push(result);
        }

        items.join("\n")
    }
}

/// A wrapper around checking the size of a file at a given URL.
///
/// It manages checking the length using the content-length header if provided,
/// or by downloading the contents if no such header exists. It also prevents
/// resource attacks by limiting the maximum size of file it will download.
pub struct CheckFileSize<'a> {
    pub url: &'a str,
    pub max_download: usize,

    client: reqwest::Client,

    size: Option<u64>,
    pub bytes: Option<bytes::Bytes>,
}

impl<'a> CheckFileSize<'a> {
    /// Create a new file size checker for a given URL, with a maximum file
    /// download size.
    pub fn new(url: &'a str, max_download: usize) -> Self {
        Self {
            url,
            max_download,
            client: reqwest::Client::new(),
            size: None,
            bytes: None,
        }
    }

    /// Get the size of the file at the URL. May download the file if the
    /// content-length header is not set.
    #[tracing::instrument(skip(self), fields(url = self.url))]
    pub async fn get_size(&mut self) -> anyhow::Result<u64> {
        if let Some(size) = self.size {
            tracing::trace!(size, "Already calculated file size");
            return Ok(size);
        }

        let data = self.client.head(self.url).send().await?;

        match data.content_length() {
            Some(content_length) if content_length > 0 => {
                tracing::debug!(
                    content_length,
                    "HEAD request yielded non-zero content-length header"
                );

                self.size = Some(content_length);
                tracing::trace!(size = content_length, "Got content-length");
                Ok(content_length)
            }
            _ => {
                tracing::debug!("HEAD request returned no content-length, downloading image");

                let bytes = self.get_bytes().await?;
                tracing::trace!(size = bytes.len(), "Downloaded bytes to calculate size");
                Ok(bytes.len() as u64)
            }
        }
    }

    /// Get the bytes at the given URL.
    #[tracing::instrument(skip(self), fields(url = self.url))]
    pub async fn get_bytes(&mut self) -> anyhow::Result<&bytes::Bytes> {
        if let Some(ref bytes) = self.bytes {
            tracing::trace!("Already downloaded file");
            return Ok(bytes);
        }

        let mut data = self.client.get(self.url).send().await?;

        let mut buf = bytes::BytesMut::new();

        while let Some(chunk) = data.chunk().await? {
            buf.extend(chunk);

            if buf.len() > self.max_download {
                tracing::warn!(
                    size = buf.len(),
                    max_download = self.max_download,
                    "Requested download is larger than max size"
                );
                anyhow::bail!("Body is larger than maximum permissible download");
            }
        }

        let bytes = buf.freeze();

        self.bytes = Some(bytes);
        Ok(self.bytes.as_ref().unwrap())
    }

    /// Consume the checker and return the bytes at the URL.
    pub async fn into_bytes(mut self) -> anyhow::Result<bytes::Bytes> {
        match self.bytes {
            Some(bytes) => Ok(bytes),
            None => {
                self.get_bytes().await?;
                Ok(self.bytes.unwrap())
            }
        }
    }
}

/// Check if an image at a provided URL is above a certain filesize. If it is,
/// download it, failing if larger than 20MB, then convert it to a 2000x2000
/// JPEG image. Convert the result into a type usable for sending via Telegram.
#[tracing::instrument]
pub async fn resize_photo(url: &str, max_size: u64) -> anyhow::Result<tgbotapi::FileType> {
    use bytes::BufMut;

    let mut check = CheckFileSize::new(url, 20_000_000);
    let size = check.get_size().await?;

    if size <= max_size {
        tracing::debug!("Photo was smaller than max size, returning existing data");

        return if let Some(bytes) = check.bytes {
            Ok(FileType::Bytes(
                format!("{}.jpg", generate_id()),
                bytes.to_vec(),
            ))
        } else {
            Ok(FileType::Url(url.to_owned()))
        };
    }

    tracing::debug!("Photo was larger than max size, resizing");

    let bytes = check.into_bytes().await?;

    let im = image::load_from_memory(&bytes)?;
    let im = im.resize(2000, 2000, image::imageops::FilterType::Lanczos3);
    let im = image::DynamicImage::ImageRgb8(im.into_rgb8());

    let mut buf = bytes::BytesMut::with_capacity(2_000_000).writer();
    im.write_to(&mut buf, image::ImageOutputFormat::Jpeg(90))?;

    let bytes = buf.into_inner().freeze().to_vec();
    Ok(FileType::Bytes(format!("{}.jpg", generate_id()), bytes))
}

/// Known resources for each language.
pub static L10N_RESOURCES: &[&str] = &["foxbot.ftl"];
/// Known languages.
pub static L10N_LANGS: &[&str] = &["en-US"];

/// A collection of language identifiers and their corresponding data.
pub type Langs = std::collections::HashMap<unic_langid::LanguageIdentifier, Vec<String>>;
/// A collection of fluent resources for a single language.
pub type LangBundle = fluent::concurrent::FluentBundle<fluent::FluentResource>;

/// Load all language data from the langs folder.
pub fn load_langs() -> Langs {
    let mut dir = std::env::current_dir().expect("Unable to get directory");
    dir.push("langs");

    let mut langs = std::collections::HashMap::new();

    for lang in L10N_LANGS {
        let path = dir.join(lang);

        let mut lang_resources = Vec::with_capacity(L10N_RESOURCES.len());
        let langid = lang
            .parse::<unic_langid::LanguageIdentifier>()
            .expect("Unable to parse language");

        for resource in L10N_RESOURCES {
            let file = path.join(resource);
            let s = std::fs::read_to_string(file).expect("Unable to read file");

            lang_resources.push(s);
        }

        langs.insert(langid, lang_resources);
    }

    langs
}

/// Get a bundle for a desired language.
pub fn get_lang_bundle(langs: &Langs, requested: &str) -> LangBundle {
    let requested_locale = match requested.parse::<unic_langid::LanguageIdentifier>() {
        Ok(locale) => locale,
        Err(err) => {
            tracing::error!("unknown locale: {:?}", err);
            L10N_LANGS[0]
                .parse::<unic_langid::LanguageIdentifier>()
                .unwrap()
        }
    };

    let requested_locales: Vec<unic_langid::LanguageIdentifier> = vec![requested_locale];
    let default_locale = L10N_LANGS[0]
        .parse::<unic_langid::LanguageIdentifier>()
        .expect("unable to parse langid");
    let available: Vec<unic_langid::LanguageIdentifier> = langs.keys().map(Clone::clone).collect();
    let resolved_locales = fluent_langneg::negotiate_languages(
        &requested_locales,
        &available,
        Some(&default_locale),
        fluent_langneg::NegotiationStrategy::Filtering,
    );

    let current_locale = resolved_locales.get(0).expect("no locales were available");

    let mut bundle =
        fluent::concurrent::FluentBundle::<fluent::FluentResource>::new(resolved_locales.clone());
    let resources = langs.get(current_locale).expect("missing known locale");

    for resource in resources {
        let resource = fluent::FluentResource::try_new(resource.to_string())
            .expect("unable to parse FTL string");
        bundle
            .add_resource(resource)
            .expect("unable to add resource");
    }

    bundle.set_use_isolating(false);

    bundle
}

pub fn get_faktory_custom() -> std::collections::HashMap<String, serde_json::Value> {
    use opentelemetry::propagation::TextMapPropagator;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let context = tracing::Span::current().context();

    let mut extra: std::collections::HashMap<String, String> = Default::default();
    let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
    propagator.inject_context(&context, &mut extra);

    extra
        .into_iter()
        .filter_map(|(key, value)| match serde_json::to_value(value) {
            Ok(val) => Some((key, val)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    fn get_finder() -> linkify::LinkFinder {
        let mut finder = linkify::LinkFinder::new();
        finder.kinds(&[linkify::LinkKind::Url]);

        finder
    }

    #[test]
    fn test_get_entity_text() {
        use super::get_entity_text;

        let url = get_entity_text(
            "hello world http://test.com",
            &tgbotapi::MessageEntity {
                entity_type: tgbotapi::MessageEntityType::Url,
                offset: 12,
                length: 15,
                url: None,
                user: None,
            },
        );
        assert_eq!(
            url, "http://test.com",
            "should be able to get url in middle of text"
        );

        let url = get_entity_text(
            "http://test.com",
            &tgbotapi::MessageEntity {
                entity_type: tgbotapi::MessageEntityType::Url,
                offset: 0,
                length: 15,
                url: None,
                user: None,
            },
        );
        assert_eq!(
            url, "http://test.com",
            "should be able to get url at start of text"
        );

        let url = get_entity_text(
            "△ http://example.com/ △",
            &tgbotapi::MessageEntity {
                entity_type: tgbotapi::MessageEntityType::Url,
                offset: 2,
                length: 19,
                url: None,
                user: None,
            },
        );
        assert_eq!(
            url, "http://example.com/",
            "should be able to get url with wide characters"
        );
    }

    #[test]
    fn test_find_links() {
        let expected_links = vec![
            "https://e621.net",
            "https://www.furaffinity.net",
            "https://www.weasyl.com",
            "https://furrynetwork.com",
        ];

        let message = tgbotapi::Message {
            text: Some("https://www.weasyl.com My message started with a link.".into()),
            caption: Some("There can also be links in the caption: https://huefox.com".into()),
            reply_markup: Some(tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: vec![
                    vec![tgbotapi::InlineKeyboardButton {
                        url: Some("https://e621.net".into()),
                        ..Default::default()
                    }],
                    vec![tgbotapi::InlineKeyboardButton {
                        url: Some("https://www.furaffinity.net".into()),
                        ..Default::default()
                    }],
                ],
            }),
            entities: Some(vec![tgbotapi::MessageEntity {
                entity_type: tgbotapi::MessageEntityType::Url,
                offset: 0,
                length: 22,
                url: Some("https://www.weasyl.com".to_string()),
                user: None,
            }]),
            caption_entities: Some(vec![tgbotapi::MessageEntity {
                entity_type: tgbotapi::MessageEntityType::TextLink,
                offset: 11,
                length: 20,
                url: Some("https://furrynetwork.com".to_string()),
                user: None,
            }]),
            ..Default::default()
        };

        let links = super::extract_links(&message);

        assert_eq!(
            links.len(),
            expected_links.len(),
            "found different number of links"
        );

        for (link, expected) in links.iter().zip(expected_links.iter()) {
            assert_eq!(link, expected);
        }
    }

    #[tokio::test]
    async fn test_link_was_seen() {
        let finder = get_finder();

        let test = "https://e621.net/post/show/934261";
        let found_links = finder.links(&test);

        let mut links = vec![];
        links.extend(found_links);
        let links: Vec<&str> = links.into_iter().map(|link| link.as_str()).collect();

        let sites: Vec<foxbot_sites::BoxedSite> = vec![
            Box::new(foxbot_sites::E621::new(
                foxbot_sites::E621Host::E621,
                "".into(),
                "".into(),
            )),
            Box::new(foxbot_sites::Mastodon::default()),
        ];

        let sites = tokio::sync::Mutex::new(sites);
        let lock = sites.lock().await;

        assert!(
            super::link_was_seen(&lock, &links, "e621.net/posts/934261"),
            "seen link was not found"
        );

        assert!(
            !super::link_was_seen(&lock, &links, "furaffinity.net/view/37137966"),
            "unseen link was found"
        );
    }

    fn matches_are_sorted(matches: &[fuzzysearch::File]) -> bool {
        matches.windows(2).all(|w| w[0].site_id <= w[1].site_id)
    }

    #[test]
    fn test_sort_results_by() {
        use super::sort_results_by;
        use foxbot_models::Sites;

        let order = Sites::default_order();

        let mut results = vec![
            fuzzysearch::File {
                site_id: 3,
                distance: Some(2),
                site_info: Some(fuzzysearch::SiteInfo::Twitter),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 2,
                distance: Some(0),
                site_info: Some(fuzzysearch::SiteInfo::Twitter),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 1,
                distance: Some(0),
                site_info: Some(fuzzysearch::SiteInfo::Weasyl),
                ..Default::default()
            },
        ];
        sort_results_by(&order, &mut results, false);
        assert!(matches_are_sorted(&results));

        let mut results = vec![
            fuzzysearch::File {
                site_id: 3,
                distance: Some(2),
                site_info: Some(fuzzysearch::SiteInfo::Twitter),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 2,
                distance: Some(0),
                site_info: Some(fuzzysearch::SiteInfo::Twitter),
                ..Default::default()
            },
            fuzzysearch::File {
                site_id: 1,
                distance: Some(2),
                site_info: Some(fuzzysearch::SiteInfo::Weasyl),
                ..Default::default()
            },
        ];
        sort_results_by(&order, &mut results, true);
        assert!(matches_are_sorted(&results));
    }
}
