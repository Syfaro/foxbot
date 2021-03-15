use anyhow::Context;
use std::sync::Arc;
use std::time::Instant;
use tracing_futures::Instrument;

use crate::models::{CachedPost, FileCache, Sites, UserConfig, UserConfigKey};
use crate::BoxedSite;

type Bundle<'a> = &'a fluent::concurrent::FluentBundle<fluent::FluentResource>;

pub struct SiteCallback<'a> {
    pub site: &'a BoxedSite,
    pub link: &'a str,
    pub duration: i64,
    pub results: Vec<crate::PostInfo>,
}

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

                let images = site
                    .get_images(user.id, link)
                    .await
                    .context("unable to extract site images")?;

                match images {
                    Some(results) => {
                        tracing::debug!(site = site.name(), "found images: {:?}", results);
                        callback(SiteCallback {
                            site: &site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                        continue 'link;
                    }
                    _ => {
                        tracing::debug!(site = site.name(), "no images found");
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

    if let Some(cached_post) = CachedPost::get(&conn, &url, thumb)
        .await
        .context("unable to get cached post")?
    {
        return Ok(ImageInfo {
            url: cached_post.cdn_url,
            dimensions: cached_post.dimensions,
        });
    }

    let im = image::load_from_memory(&data)?;

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
    s3.put_object(put).await.unwrap();

    let cdn_url = format!("{}/{}/{}", s3_url, s3_bucket, key);

    if let Err(err) = CachedPost::save(&conn, &url, &cdn_url, thumb, dimensions).await {
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
    use bytes::BufMut;

    let mut resp = reqwest::get(url).await?;
    let content_length = resp.content_length().unwrap_or(2_000_000);
    if resp.content_length().unwrap_or_default() > 50_000_000 {
        return Err(anyhow::anyhow!("content length is too large"));
    }

    let mut data = bytes::BytesMut::with_capacity(content_length as usize);
    let mut size = 0;

    while let Some(chunk) = resp.chunk().await? {
        size += chunk.len();
        if size > 50_000_000 {
            return Err(anyhow::anyhow!("body was too long"));
        }
        data.put(chunk);
    }

    Ok(data.freeze())
}

/// Download URL from post and calculate image dimensions, returning a new
/// PostInfo.
#[tracing::instrument(skip(data))]
pub async fn size_post(
    post: &crate::PostInfo,
    data: &bytes::Bytes,
) -> anyhow::Result<crate::PostInfo> {
    use image::GenericImageView;

    // If we already have image dimensions, assume they're valid and reuse.
    if post.image_dimensions.is_some() {
        return Ok(post.to_owned());
    }

    let im = image::load_from_memory(&data)?;
    let dimensions = im.dimensions();

    Ok(crate::PostInfo {
        image_dimensions: Some(dimensions),
        image_size: Some(data.len()),
        ..post.to_owned()
    })
}

/// Download URL from post, calculate image dimensions, convert to JPEG and
/// generate thumbnail, and upload to S3 bucket. Returns a new PostInfo with
/// the updated URLs and dimensions.
#[tracing::instrument(err, skip(conn, s3, s3_bucket, s3_url, data))]
pub async fn cache_post(
    conn: &sqlx::Pool<sqlx::Postgres>,
    s3: &rusoto_s3::S3Client,
    s3_bucket: &str,
    s3_url: &str,
    post: &crate::PostInfo,
    data: &bytes::Bytes,
) -> anyhow::Result<crate::PostInfo> {
    let image = upload_image(&conn, &s3, &s3_bucket, &s3_url, &post.url, false, &data).await?;

    let thumb = if let Some(thumb) = &post.thumb {
        Some(
            upload_image(&conn, &s3, &s3_bucket, &s3_url, &thumb, true, &data)
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

pub fn add_sentry_tracing(scope: &mut sentry::Scope) {
    use opentelemetry::api::HttpTextFormat;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let current_context = tracing::Span::current().context();

    let mut carrier = std::collections::HashMap::new();
    let propagator = opentelemetry::api::B3Propagator::new(true);
    propagator.inject_context(&current_context, &mut carrier);

    let data: &str = carrier.get("X-B3").unwrap();
    scope.set_extra("x-b3", data.to_owned().into());
}

type SentryTags<'a> = Option<Vec<(&'a str, String)>>;

pub fn with_user_scope<C, R>(from: Option<&tgbotapi::User>, tags: SentryTags, callback: C) -> R
where
    C: FnOnce() -> R,
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
        s.push_str(&get_message(&bundle, "alternate-posted-by", Some(args)).unwrap());
        s.push('\n');
        let mut subs: Vec<fuzzysearch::File> = item.1.to_vec();
        subs.sort_by(|a, b| a.id.partial_cmp(&b.id).unwrap());
        subs.dedup_by(|a, b| a.id == b.id);
        subs.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());
        for sub in subs {
            tracing::trace!(site = sub.site_name(), id = sub.id, "looking at submission");
            let mut args = fluent::FluentArgs::new();
            args.insert("link", sub.url().into());
            args.insert("distance", sub.distance.unwrap_or(10).into());
            let rating = get_message(&bundle, get_rating_bundle_name(&sub.rating), None).unwrap();
            args.insert("rating", rating.into());
            s.push_str(&get_message(&bundle, "alternate-distance", Some(args)).unwrap());
            s.push('\n');
            used_hashes.push(sub.hash.unwrap());
        }
        s.push('\n');
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

    tracing::trace!(from_id = from.id, "evaluating if known bot");

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
    use futures::StreamExt;
    use std::time::Duration;
    use tokio_stream::wrappers::IntervalStream;

    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(
        async move {
            let chat_action = tgbotapi::requests::SendChatAction { chat_id, action };

            let mut count: usize = 0;

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
                            with_user_scope(user.as_ref(), None, || {
                                sentry::capture_error(&e);
                            });
                        }
                    }),
            );

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

impl Drop for ContinuousAction {
    fn drop(&mut self) {
        let tx = std::mem::replace(&mut self.tx, None);
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
    }
}

#[tracing::instrument(err, skip(bot, conn, fapi))]
pub async fn match_image(
    bot: &tgbotapi::Telegram,
    conn: &sqlx::Pool<sqlx::Postgres>,
    fapi: &fuzzysearch::FuzzySearch,
    file: &tgbotapi::PhotoSize,
    distance: Option<i64>,
) -> anyhow::Result<Vec<fuzzysearch::File>> {
    if let Some(hash) = FileCache::get(&conn, &file.file_unique_id)
        .await
        .context("unable to query file cache")?
    {
        return lookup_single_hash(&fapi, hash, distance).await;
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

    FileCache::set(&conn, &file.file_unique_id, hash)
        .await
        .context("unable to set file cache")?;

    lookup_single_hash(&fapi, hash, distance).await
}

async fn lookup_single_hash(
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

    Ok(matches)
}

pub async fn sort_results(
    conn: &sqlx::Pool<sqlx::Postgres>,
    user_id: i64,
    results: &mut Vec<fuzzysearch::File>,
) -> anyhow::Result<()> {
    // If we have 1 or fewer items, we don't need to do any sorting.
    if results.len() <= 1 {
        return Ok(());
    }

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
                    links.push(&url);
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
            links.push(get_entity_text(&text, &entity));
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
    let start = utf16_pos_in_utf8(&text, entity.offset as usize);
    let end = utf16_pos_in_utf8(&text, (entity.offset + entity.length) as usize);

    &text[start..end]
}

/// Check if a link was contained within a linkify Link.
pub fn link_was_seen(
    sites: &tokio::sync::MutexGuard<Vec<crate::BoxedSite>>,
    links: &[&str],
    source: &str,
) -> bool {
    // Find the unique ID for the source link. If one does not exist, we can't
    // find any matches against it.
    let source_id = match sites.iter().find_map(|site| site.url_id(&source)) {
        Some(source) => source,
        _ => return false,
    };

    links.iter().any(
        |link| match sites.iter().find_map(|site| site.url_id(&link)) {
            Some(link_id) => {
                tracing::debug!("{} - {}", link, link_id);
                link_id == source_id
            }
            _ => false,
        },
    )
}

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
        } => Some(&from),
        Update {
            callback_query: Some(CallbackQuery { from, .. }),
            ..
        } => Some(&from),
        _ => None,
    }
}

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

pub async fn can_delete_in_chat(
    bot: &tgbotapi::Telegram,
    conn: &sqlx::Pool<sqlx::Postgres>,
    chat_id: i64,
    user_id: i64,
    ignore_cache: bool,
) -> anyhow::Result<bool> {
    use crate::models::{GroupConfig, GroupConfigKey};

    // If we're not ignoring cache, start by checking if we already have a value
    // in the database.
    if !ignore_cache {
        let can_delete: Option<bool> =
            GroupConfig::get(&conn, chat_id, GroupConfigKey::HasDeletePermission).await?;

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
        &conn,
        GroupConfigKey::HasDeletePermission,
        chat_id,
        can_delete,
    )
    .await?;

    Ok(can_delete)
}

pub fn get_rating_bundle_name(rating: &Option<fuzzysearch::Rating>) -> &'static str {
    match rating {
        Some(fuzzysearch::Rating::General) => "rating-general",
        Some(fuzzysearch::Rating::Mature) | Some(fuzzysearch::Rating::Adult) => "rating-adult",
        None => "rating-unknown",
    }
}

pub fn source_reply(matches: &[fuzzysearch::File], bundle: Bundle<'_>) -> String {
    let first = match matches.first() {
        Some(result) => result,
        None => return get_message(&bundle, "reverse-no-results", None).unwrap(),
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

        let rating = get_rating_bundle_name(&first.rating);
        let rating = get_message(&bundle, rating, None).unwrap();
        args.insert("rating", rating.into());

        get_message(&bundle, "reverse-result", Some(args)).unwrap()
    } else {
        let mut items = Vec::with_capacity(2 + similar.len());

        let text = get_message(&bundle, "reverse-multiple-results", None).unwrap();
        items.push(text);

        for file in vec![first].into_iter().chain(similar) {
            let mut args = fluent::FluentArgs::new();
            args.insert("link", file.url().into());

            let rating = get_rating_bundle_name(&file.rating);
            let rating = get_message(&bundle, rating, None).unwrap();
            args.insert("rating", rating.into());

            let result = get_message(&bundle, "reverse-multiple-item", Some(args)).unwrap();
            items.push(result);
        }

        items.join("\n")
    }
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

        let sites: Vec<crate::BoxedSite> = vec![
            Box::new(crate::sites::E621::new()),
            Box::new(crate::sites::Mastodon::new()),
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
}
