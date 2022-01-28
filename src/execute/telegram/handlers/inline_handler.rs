use std::{borrow::Cow, sync::Arc};

use async_trait::async_trait;
use fluent_bundle::FluentArgs;
use futures::{stream::FuturesOrdered, StreamExt};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tgbotapi::{requests::*, Command, FileType, InlineKeyboardButton, InlineKeyboardMarkup};

use crate::{
    execute::{telegram::Context, telegram_jobs::CoconutEventJob},
    models, needs_field,
    sites::PostInfo,
    utils, Error,
};

use super::{
    Handler,
    Status::{self, *},
};

/// Maximum size of inline image before resizing.
static MAX_IMAGE_SIZE: usize = 5_000_000;

/// Maximum size of video allowed to be returned in inline result.
static MAX_VIDEO_SIZE: i32 = 10_000_000;

pub struct InlineHandler;

#[derive(Clone)]
pub enum ResultType {
    Ready,
    VideoToBeProcessed,
    VideoTooLarge,
}

impl ResultType {
    fn is_video(&self) -> bool {
        matches!(self, Self::VideoToBeProcessed | Self::VideoTooLarge)
    }

    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SentAs {
    Video(String),
    Animation(String),
    Document(String),
}

impl InlineHandler {
    async fn process_video(&self, cx: &Context, message: &tgbotapi::Message) -> Result<(), Error> {
        let text = message.text.as_ref().unwrap();
        let display_name = match text.split('-').nth(1) {
            Some(id) => id,
            None => return Ok(()),
        };

        let video = models::Video::lookup_by_display_name(&cx.pool, display_name)
            .await?
            .ok_or_else(|| Error::missing("video missing for processing"))?;

        let bundle = cx.get_fluent_bundle(message).await;

        if video.processed {
            if let Some(video_url) = video.mp4_url {
                tracing::debug!("already had video url, assuming large and sending");

                let text = utils::get_message(&bundle, "inline-source", None);

                let reply_markup = ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![InlineKeyboardButton {
                        text,
                        url: Some(video.display_url.to_owned()),
                        ..Default::default()
                    }]],
                });

                if let Ok(Some(sent_as)) = get_cached_video(&cx.redis, display_name).await {
                    if send_previous_video(
                        cx,
                        &sent_as,
                        message.chat_id(),
                        reply_markup.clone(),
                        video.height.unwrap_or_default(),
                        video.width.unwrap_or_default(),
                        video.duration.unwrap_or_default(),
                    )
                    .await
                    .is_ok()
                    {
                        return Ok(());
                    }
                }

                let action = utils::continuous_action(
                    cx.bot.clone(),
                    10,
                    message.chat_id(),
                    ChatAction::UploadVideo,
                );

                let data = reqwest::get(video_url).await?.bytes().await?;

                let send_video = SendVideo {
                    chat_id: message.chat_id(),
                    video: FileType::Bytes(format!("{}.mp4", video.display_name), data.to_vec()),
                    reply_markup: Some(reply_markup),
                    height: video.height,
                    width: video.width,
                    duration: video.duration,
                    supports_streaming: Some(true),
                    ..Default::default()
                };

                let message = cx.make_request(&send_video).await?;
                if let Some(sent_as) = find_sent_as(&message) {
                    cache_video(&cx.redis, display_name, &sent_as).await?;
                }

                drop(action);

                return Ok(());
            } else {
                tracing::warn!("video processed but no url");
            }
        }

        if video.job_id.is_none() {
            let job_id = cx
                .coconut
                .start_video(&video.url, &video.display_name)
                .await?;

            models::Video::set_job_id(&cx.pool, video.id, &job_id).await?;
        }

        let video_starting = utils::get_message(&bundle, "video-starting", None);

        let send_message = SendMessage {
            chat_id: message.chat_id(),
            text: video_starting,
            ..Default::default()
        };
        let sent = cx.make_request(&send_message).await?;

        models::Video::add_message_id(&cx.pool, video.id, &sent.chat, sent.message_id).await?;

        Ok(())
    }
}

/// Get links recently searched by the given user.
///
/// This also resets the expire time of the set of recent links and removes
/// expired entries.
async fn get_inline_history(
    user: i64,
    redis: &mut redis::aio::ConnectionManager,
) -> Result<Vec<String>, Error> {
    let key = format!("inline-history:{}", user);

    let now = chrono::Utc::now().timestamp();

    let links: Vec<String> = redis
        .zrevrangebyscore_limit(&key, "+inf", now, 0, 50)
        .await?;
    redis.zrembyscore(&key, "-inf", now).await?;
    redis.expire(key, 60 * 60 * 24).await?;

    Ok(links)
}

/// Add links to a user's inline query history.
async fn add_inline_history(
    user: i64,
    links: &[Cow<'_, str>],
    redis: &mut redis::aio::ConnectionManager,
) -> Result<(), Error> {
    let key = format!("inline-history:{}", user);

    let expires_at = (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp();

    let items: Vec<(i64, &str)> = links
        .iter()
        .map(|link| (expires_at, link.as_ref()))
        .collect();
    redis
        .zadd_multiple::<_, i64, &str, ()>(&key, &items)
        .await?;
    redis.expire(key, 60 * 60 * 24).await?;

    Ok(())
}

#[async_trait]
impl Handler for InlineHandler {
    fn name(&self) -> &'static str {
        "inline"
    }

    fn add_jobs(&self, worker_environment: &mut super::WorkerEnvironment) {
        worker_environment.register::<CoconutEventJob, _, _, _>(dispatch_event);
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        if let Some(message) = &update.message {
            match message.get_command() {
                Some(cmd) if cmd.name == "/start" => {
                    if let Some(text) = &message.text {
                        if text.contains("process-") {
                            self.process_video(cx, message).await?;
                            return Ok(Completed);
                        }
                    }
                }
                _ => (),
            }
        }

        let inline = needs_field!(update, inline_query);

        let bundle = cx
            .get_fluent_bundle(inline.from.language_code.as_deref())
            .await;

        let mut redis = cx.redis.clone();

        let history_enabled =
            models::UserConfig::get(&cx.pool, models::UserConfigKey::InlineHistory, &inline.from)
                .await
                .unwrap_or_default()
                .unwrap_or(false);
        tracing::debug!("user has history enabled: {}", history_enabled);

        let links: Vec<Cow<'_, str>> = if history_enabled && inline.query.is_empty() {
            match get_inline_history(inline.from.id, &mut redis).await {
                Ok(history) => {
                    tracing::trace!("got inline history with {} items", history.len());
                    history.into_iter().map(Into::into).collect()
                }
                Err(err) => {
                    tracing::error!("could not get inline history: {}", err);
                    vec![]
                }
            }
        } else {
            let links: Vec<_> = cx
                .finder
                .links(&inline.query)
                .map(|link| link.as_str().into())
                .collect();

            if history_enabled {
                if let Err(err) = add_inline_history(inline.from.id, &links, &mut redis).await {
                    tracing::error!("could not add inline history: {}", err);
                }
            }

            links
        };

        let mut results: Vec<PostInfo> = Vec::new();

        tracing::info!(query = ?inline.query, "got query");
        tracing::debug!(?links, "found links");

        // Lock sites in order to find which of these links are usable
        let images_err = {
            let mut sites = cx.sites.lock().await;
            let links = links.iter().map(|link| link.as_ref()).collect();
            utils::find_images(&inline.from, links, &mut sites, &cx.redis, &mut |info| {
                results.extend(info.results);
            })
            .await
            .err()
        };

        if let Some(err) = images_err {
            let article = if let Error::UserMessage { message, .. } = &err {
                tracing::warn!(
                    "got displayable error message, sending to user: {:?}",
                    message
                );

                InlineQueryResult::article(
                    utils::generate_id(),
                    format!("Error: {}", message),
                    message.to_owned().to_string(),
                )
            } else {
                tracing::error!("got non-displayable error message: {:?}", err);

                InlineQueryResult::article(
                    utils::generate_id(),
                    "Error".into(),
                    "Unknown error".into(),
                )
            };

            let answer_inline = AnswerInlineQuery {
                inline_query_id: inline.id.to_owned(),
                results: vec![article],
                ..Default::default()
            };

            cx.make_request(&answer_inline).await?;

            return Ok(Completed);
        }

        let is_personal = results.iter().any(|result| result.personal);

        let include_tags = inline.query.contains("#tags");
        let include_info = inline.query.contains("#info") || include_tags;

        let mut futs: FuturesOrdered<_> = results
            .iter()
            .map(|result| process_result(cx, result, &inline.from, include_info, include_tags))
            .collect();

        let mut responses: Vec<(ResultType, InlineQueryResult)> = vec![];
        while let Some(item) = futs.next().await {
            if let Ok(Some(items)) = item {
                responses.extend(items);
            }
        }

        // If we had no responses but the query was not empty, there were likely links
        // that we were unable to convert. We need to display that the links had no results.
        if responses.is_empty() && !inline.query.is_empty() {
            let article = InlineQueryResult::article(
                utils::generate_id(),
                utils::get_message(&bundle, "inline-no-results-title", None),
                utils::get_message(&bundle, "inline-no-results-body", None),
            );

            responses.push((ResultType::Ready, article));
        }

        // Check if we need to process any videos contained within the results.
        // These don't get returned as regular inline results.
        let has_video: Option<(ResultType, InlineQueryResult)> = responses
            .iter()
            .find(|(result_type, _result)| result_type.is_video())
            .map(|(result_type, result)| (result_type.clone(), result.clone()));

        // Get the rest of the ready results which should still be displayed.
        let cleaned_responses = responses
            .into_iter()
            .filter(|(result_type, _result)| result_type.is_ready())
            .map(|(_result_type, result)| result)
            .collect();

        let mut answer_inline = AnswerInlineQuery {
            inline_query_id: inline.id.to_owned(),
            results: cleaned_responses,
            is_personal: Some(is_personal),
            cache_time: Some(0),
            ..Default::default()
        };

        if inline.query.is_empty() {
            if answer_inline.results.is_empty() {
                // If the query was empty, display a help button to make it easy to get
                // started using the bot.
                let help_text = utils::get_message(&bundle, "inline-help", None);

                answer_inline.switch_pm_text = Some(help_text);
                answer_inline.switch_pm_parameter = Some("help".to_string());
            } else {
                // If the results were not empty, there's personal content here.
                answer_inline.is_personal = Some(true);
            }
        }

        // If we had a video that needed to be processed, replace the switch pm
        // parameters to go and process that video.
        if let Some((result_type, result)) = has_video {
            let text = match result_type {
                ResultType::VideoToBeProcessed => {
                    utils::get_message(&bundle, "inline-process", None)
                }
                ResultType::VideoTooLarge => {
                    utils::get_message(&bundle, "inline-large-video", None)
                }
                _ => unreachable!("only video results should be found"),
            };

            answer_inline.switch_pm_text = Some(text);
            answer_inline.switch_pm_parameter = Some(result.id);
        }

        cx.make_request(&answer_inline).await?;

        Ok(Completed)
    }
}

async fn dispatch_event(
    cx: Arc<Context>,
    _job: faktory::Job,
    event: CoconutEventJob,
) -> Result<(), Error> {
    match event {
        CoconutEventJob::Progress {
            display_name,
            progress,
        } => video_progress(&cx, &display_name, &progress).await?,
        CoconutEventJob::Completed {
            display_name,
            video_url,
            thumb_url,
            video_size,
            height,
            width,
            duration,
        } => {
            video_complete(
                &cx,
                &display_name,
                &video_url,
                &thumb_url,
                video_size,
                height,
                width,
                duration,
            )
            .await?
        }
        CoconutEventJob::Failed { display_name } => {
            tracing::warn!("got failed coconut job for {}", display_name);
        }
    }

    Ok(())
}

async fn send_video_progress(cx: &Context, display_name: &str, message: &str) -> Result<(), Error> {
    tracing::info!("sending video progress");

    let video = models::Video::lookup_by_display_name(&cx.pool, display_name)
        .await?
        .ok_or_else(|| Error::missing("video for progress"))?;

    let messages = models::Video::associated_messages(&cx.pool, video.id).await?;

    for (chat_id, message_id) in messages {
        let edit_message = EditMessageText {
            chat_id: chat_id.into(),
            message_id: Some(message_id),
            text: message.to_owned(),
            ..Default::default()
        };

        if let Err(err) = cx.make_request(&edit_message).await {
            tracing::error!("could not send video progress message: {}", err);
        }
    }

    Ok(())
}

async fn video_progress(cx: &Context, display_name: &str, progress: &str) -> Result<(), Error> {
    tracing::info!("got video progress");

    let bundle = cx.get_fluent_bundle(None).await;

    let mut args = FluentArgs::new();
    args.set("percent", progress.to_string());

    let message = utils::get_message(&bundle, "video-progress", Some(args));

    send_video_progress(cx, display_name, &message).await?;

    Ok(())
}

#[tracing::instrument(skip(cx), fields(video_id))]
async fn video_complete(
    cx: &Context,
    display_name: &str,
    video_url: &str,
    thumb_url: &str,
    video_size: i32,
    height: i32,
    width: i32,
    duration: i32,
) -> Result<(), Error> {
    tracing::info!("video completed");

    // Coconut only sends 100% progress as part of video.completed
    video_progress(cx, display_name, "100%").await?;

    let video = models::Video::lookup_by_display_name(&cx.pool, display_name)
        .await?
        .ok_or_else(|| Error::missing("video for complete"))?;
    tracing::Span::current().record("video_id", &video.id);

    models::Video::set_processed_url(
        &cx.pool, video.id, video_url, thumb_url, video_size, height, width,
    )
    .await?;

    let mut messages = models::Video::associated_messages(&cx.pool, video.id).await?;

    tracing::debug!(count = messages.len(), "found messages associated with job");

    let first = match messages.pop() {
        Some(msg) => msg,
        None => return Ok(()),
    };

    let bundle = cx.get_fluent_bundle(None).await;

    let video_too_large = utils::get_message(&bundle, "video-too-large", None);
    let video_source_button = utils::get_message(&bundle, "inline-source", None);
    let video_return_button = utils::get_message(&bundle, "video-return-button", None);

    // First, we should handle one message to upload the file. This is a
    // special case because afterwards all additional messages can be sent
    // the existing file ID instead of a new upload.

    let video_is_large = video_size > MAX_VIDEO_SIZE;

    if video_is_large {
        let send_message = tgbotapi::requests::SendMessage {
            chat_id: first.0.into(),
            text: video_too_large.clone(),
            disable_notification: Some(true),
            ..Default::default()
        };

        cx.make_request(&send_message).await?;
    }

    let action =
        utils::continuous_action(cx.bot.clone(), 6, first.0.into(), ChatAction::UploadVideo);

    let data = reqwest::get(video_url).await?.bytes().await?;

    let send_video = if video_is_large {
        SendVideo {
            chat_id: first.0.into(),
            video: FileType::Bytes(format!("{}.mp4", display_name), data.to_vec()),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text: video_source_button.clone(),
                    url: Some(video.display_url.to_owned()),
                    ..Default::default()
                }]],
            })),
            supports_streaming: Some(true),
            height: Some(height),
            width: Some(width),
            duration: Some(duration),
            ..Default::default()
        }
    } else {
        SendVideo {
            chat_id: first.0.into(),
            video: FileType::Bytes(format!("{}.mp4", display_name), data.to_vec()),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text: video_return_button.clone(),
                    switch_inline_query: Some(video.display_url.to_owned()),
                    ..Default::default()
                }]],
            })),
            supports_streaming: Some(true),
            height: Some(height),
            width: Some(width),
            duration: Some(duration),
            ..Default::default()
        }
    };

    let message = cx.make_request(&send_video).await?;
    drop(action);

    if messages.is_empty() {
        return Ok(());
    }

    // Now that we've sent the first message, send each additional message
    // with the returned video ID.

    let sent_as = find_sent_as(&message)
        .ok_or_else(|| Error::bot("sent video was missing all known file id types"))?;

    cache_video(&cx.redis, display_name, &sent_as).await?;

    tracing::debug!(?sent_as, "sent video, reusing for additional messages");

    let button = if video_is_large {
        InlineKeyboardButton {
            text: video_source_button.clone(),
            url: Some(video.display_url.clone()),
            ..Default::default()
        }
    } else {
        InlineKeyboardButton {
            text: video_return_button.clone(),
            switch_inline_query: Some(video.display_url.to_owned()),
            ..Default::default()
        }
    };

    let reply_markup = ReplyMarkup::InlineKeyboardMarkup(InlineKeyboardMarkup {
        inline_keyboard: vec![vec![button]],
    });

    for message in messages {
        if video_is_large {
            let send_message = SendMessage {
                chat_id: message.0.into(),
                text: video_too_large.clone(),
                disable_notification: Some(true),
                ..Default::default()
            };

            if let Err(err) = cx.make_request(&send_message).await {
                tracing::error!("could not send too large message: {}", err);
            }
        }

        if let Err(err) = send_previous_video(
            cx,
            &sent_as,
            message.0.into(),
            reply_markup.clone(),
            height,
            width,
            duration,
        )
        .await
        {
            tracing::error!("unable to send video: {:?}", err);
        }
    }

    tracing::debug!("finished handling video");

    Ok(())
}

/// Convert a [PostInfo] struct into an InlineQueryResult.
///
/// It adds an inline keyboard for the direct link and source if available.
async fn process_result(
    cx: &Context,
    result: &PostInfo,
    from: &tgbotapi::User,
    include_info: bool,
    include_tags: bool,
) -> Result<Option<Vec<(ResultType, InlineQueryResult)>>, Error> {
    let bundle = cx.get_fluent_bundle(from.language_code.as_deref()).await;

    let direct = utils::get_message(&bundle, "inline-direct", None);

    let mut row = vec![InlineKeyboardButton {
        text: direct,
        url: Some(result.url.clone()),
        callback_data: None,
        ..Default::default()
    }];

    if let Some(source_link) = &result.source_link {
        let text = result.site_name.to_string();

        row.push(InlineKeyboardButton {
            text,
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
        "png" | "jpeg" | "jpg" => Ok(Some(
            build_image_result(cx, result, thumb_url, &keyboard, include_info, include_tags)
                .await?,
        )),
        "webm" => {
            let source = match &result.source_link {
                Some(link) => link.to_owned(),
                None => result.url.clone(),
            };

            let url_id = {
                let sites = cx.sites.lock().await;
                sites
                    .iter()
                    .find_map(|site| site.url_id(&source))
                    .ok_or_else(|| Error::missing("site url_id"))?
            };

            let results =
                build_webm_result(&cx.pool, result, thumb_url, &keyboard, url_id, &source)
                    .await
                    .expect("unable to process webm results");

            Ok(Some(results))
        }
        "mp4" => Ok(Some(build_mp4_result(result, thumb_url, &keyboard))),
        "gif" => Ok(Some(build_gif_result(result, thumb_url, &keyboard))),
        other => {
            tracing::warn!(file_type = other, "got unusable type");
            Ok(None)
        }
    }
}

fn escape_markdown<S: AsRef<str>>(input: S) -> String {
    input
        .as_ref()
        .replace("_", r"\_")
        .replace("-", r"\-")
        .replace("!", r"\!")
        .replace(".", r"\.")
        .replace("(", r"\(")
        .replace(")", r"\)")
        .replace("[", r"\[")
        .replace("]", r"\]")
        .replace("{", r"\{")
        .replace("}", r"\}")
        .replace("*", r"\*")
        .replace("#", r"\#")
        .replace("`", r"\`")
        .replace("+", r"\+")
        .replace("=", r"\=")
        .replace(">", r"\>")
        .replace("|", r"\|")
        .replace("~", r"\~")
}

#[derive(Debug, Deserialize, Serialize)]
struct URLCacheData {
    /// URL to send for this image. May be original or cached image.
    sent_url: String,
    /// URL to send for this image's thumbnail. May be original or cached image.
    sent_thumb_url: Option<String>,
    /// Dimensions of this image.
    dimensions: (u32, u32),
}

/// Get information about a URL from the cache, if it exists.
async fn get_url_cache_data(
    url: &str,
    redis: &mut redis::aio::ConnectionManager,
) -> Result<Option<URLCacheData>, Error> {
    let mut hasher = Sha256::new();
    hasher.update(url);
    let result = base64::encode(hasher.finalize());

    let key = format!("url:{}", result);

    redis
        .get::<_, Option<Vec<u8>>>(key)
        .await?
        .map(|data| serde_json::from_slice(&data))
        .transpose()
        .map_err(Into::into)
}

/// Set cached information about an image URL.
async fn set_url_cache_data(
    url: &str,
    post_info: &PostInfo,
    redis: &mut redis::aio::ConnectionManager,
) -> Result<(), Error> {
    let dimensions = match post_info.image_dimensions {
        Some(dimensions) => dimensions,
        None => return Err(Error::bot("post info was missing dimensions")),
    };

    let data = serde_json::to_vec(&URLCacheData {
        sent_url: post_info.url.clone(),
        sent_thumb_url: post_info.thumb.clone(),
        dimensions,
    })?;

    let mut hasher = Sha256::new();
    hasher.update(url);
    let result = base64::encode(hasher.finalize());

    let key = format!("url:{}", result);
    redis.set_ex(key, data, 60 * 60 * 24).await?;

    Ok(())
}

async fn build_image_result(
    cx: &Context,
    result: &PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
    include_info: bool,
    include_tags: bool,
) -> Result<Vec<(ResultType, InlineQueryResult)>, Error> {
    let mut result = result.to_owned();
    result.thumb = Some(thumb_url);

    // There is a bit of processing required to figure out how to handle an
    // image before sending it off to Telegram. First, we check if the config
    // specifies we should cache (re-upload) all images to the S3 bucket. If
    // enabled, this is all we have to do. Otherwise, we need to download the
    // image to make sure that it is below Telegram's 5MB image limit. This also
    // allows us to calculate an image height and width to resolve a bug in
    // Telegram Desktop[^1].
    //
    // [^1]: https://github.com/telegramdesktop/tdesktop/issues/4580

    let mut redis = cx.redis.clone();
    let result = if let Ok(Some(cache_data)) = get_url_cache_data(&result.url, &mut redis).await {
        tracing::trace!("had cached data for full url");

        PostInfo {
            url: cache_data.sent_url,
            thumb: cache_data.sent_thumb_url,
            image_dimensions: Some(cache_data.dimensions),
            ..result
        }
    } else {
        let data = utils::download_image(&result.url).await?;

        let result = {
            let result = utils::size_post(&result, &data).await?;

            if result.image_size.unwrap_or_default() > MAX_IMAGE_SIZE {
                utils::cache_post(
                    &cx.pool,
                    &cx.s3,
                    &cx.config.s3_bucket,
                    &cx.config.s3_url,
                    &result,
                    &data,
                )
                .await?
            } else {
                result
            }
        };

        if let Err(err) = set_url_cache_data(&result.url, &result, &mut redis).await {
            tracing::error!("could not cache url data: {}", err);
        }

        result
    };

    let mut photo = InlineQueryResult::photo(
        utils::generate_id(),
        result.url.to_owned(),
        result.thumb.clone().unwrap(),
    );

    let mut data = Vec::new();

    if include_info {
        if let Some(title) = result.title {
            data.push(format!("Title: {}", escape_markdown(title)));
        }

        match (result.artist_username, result.artist_url) {
            (Some(username), None) => data.push(format!("Artist: {}", escape_markdown(username))),
            (Some(username), Some(url)) => {
                data.push(format!("Artist: [{}]({})", escape_markdown(username), url))
            }
            (None, Some(url)) => data.push(format!("Artist: {}", escape_markdown(url))),
            (None, None) => (),
        }
    }

    if include_tags {
        match result.tags {
            Some(tags) if !tags.is_empty() => {
                data.push(format!(
                    "Tags: {}",
                    tags.iter()
                        // Tags can't be all numbers, and no transformation can fix that
                        .filter(|tag| tag.parse::<i64>().is_err())
                        // Escape tags for Markdown formatting and replace unsupported symbols
                        .map(|tag| escape_markdown(format!(
                            "#{}",
                            tag.replace(' ', "_")
                                .replace('/', "_")
                                .replace('(', "")
                                .replace(')', "")
                                .replace('-', "_")
                        )))
                        .take(50)
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            _ => (),
        }
    }

    if !data.is_empty() {
        if let InlineQueryType::Photo(ref mut photo) = photo.content {
            photo.parse_mode = Some(ParseMode::MarkdownV2);
            photo.caption = Some(data.join("\n"));
        }
    }

    photo.reply_markup = Some(keyboard.clone());

    if let Some(dims) = result.image_dimensions {
        if let InlineQueryType::Photo(ref mut photo) = photo.content {
            photo.photo_width = Some(dims.0);
            photo.photo_height = Some(dims.1);
        }
    }

    let mut results = vec![(ResultType::Ready, photo)];

    if let Some(message) = &result.extra_caption {
        let mut photo =
            InlineQueryResult::photo(utils::generate_id(), result.url, result.thumb.unwrap());
        photo.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Photo(ref mut photo) = photo.content {
            photo.caption = Some(message.to_string());

            if let Some(dims) = result.image_dimensions {
                photo.photo_width = Some(dims.0);
                photo.photo_height = Some(dims.1);
            }
        }

        results.push((ResultType::Ready, photo));
    };

    Ok(results)
}

async fn build_webm_result(
    conn: &sqlx::Pool<sqlx::Postgres>,
    result: &PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
    url_id: String,
    display_url: &str,
) -> Result<Vec<(ResultType, InlineQueryResult)>, Error> {
    let video = match models::Video::lookup_by_url_id(conn, &url_id).await? {
        None => {
            let display_name = models::Video::insert_media(
                conn,
                &url_id,
                &result.url,
                display_url,
                &utils::generate_id(),
            )
            .await?;

            return Ok(vec![(
                ResultType::VideoToBeProcessed,
                InlineQueryResult::article(
                    format!("process-{}", display_name),
                    "".into(),
                    "".into(),
                ),
            )]);
        }
        Some(video) if !video.processed => {
            return Ok(vec![(
                ResultType::VideoToBeProcessed,
                InlineQueryResult::article(
                    format!("process-{}", video.display_name),
                    "".into(),
                    "".into(),
                ),
            )]);
        }
        Some(models::Video {
            display_name,
            file_size: Some(file_size),
            ..
        }) if file_size > MAX_VIDEO_SIZE => {
            return Ok(vec![(
                ResultType::VideoTooLarge,
                InlineQueryResult::article(
                    format!("process-{}", display_name),
                    "".into(),
                    "".into(),
                ),
            )]);
        }
        Some(video) => video,
    };

    let full_url = video.mp4_url.unwrap();

    let mut video_result = InlineQueryResult::video(
        video.display_name.clone(),
        full_url.to_owned(),
        "video/mp4".to_owned(),
        thumb_url.to_owned(),
        result.url.clone(),
    );
    video_result.reply_markup = Some(keyboard.clone());

    let mut results = vec![(ResultType::Ready, video_result)];

    if let Some(message) = &result.extra_caption {
        let mut video_result = InlineQueryResult::video(
            video.display_name,
            full_url,
            "video/mp4".to_owned(),
            thumb_url,
            result.url.clone(),
        );
        video_result.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Video(ref mut result) = video_result.content {
            result.caption = Some(message.to_string());
        }

        results.push((ResultType::Ready, video_result));
    };

    Ok(results)
}

fn build_mp4_result(
    result: &PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
) -> Vec<(ResultType, InlineQueryResult)> {
    let full_url = result.url.clone();

    let mut video = InlineQueryResult::video(
        utils::generate_id(),
        full_url,
        "video/mp4".to_string(),
        thumb_url,
        result
            .title
            .clone()
            .unwrap_or_else(|| result.site_name.to_owned().into()),
    );
    video.reply_markup = Some(keyboard.clone());

    if let Some(message) = &result.extra_caption {
        if let InlineQueryType::Video(ref mut result) = video.content {
            result.caption = Some(message.to_owned());
        }
    }

    vec![(ResultType::Ready, video)]
}

fn build_gif_result(
    result: &PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
) -> Vec<(ResultType, InlineQueryResult)> {
    let full_url = result.url.clone();

    let mut gif = InlineQueryResult::gif(
        utils::generate_id(),
        full_url.to_owned(),
        thumb_url.to_owned(),
    );
    gif.reply_markup = Some(keyboard.clone());

    let mut results = vec![(ResultType::Ready, gif)];

    if let Some(message) = &result.extra_caption {
        let mut gif = InlineQueryResult::gif(utils::generate_id(), full_url, thumb_url);
        gif.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Gif(ref mut result) = gif.content {
            result.caption = Some(message.to_string());
        }

        results.push((ResultType::Ready, gif));
    };

    results
}

#[allow(clippy::manual_map)]
fn find_sent_as(message: &tgbotapi::Message) -> Option<SentAs> {
    if let Some(video) = &message.video {
        Some(SentAs::Video(video.file_id.clone()))
    } else if let Some(animation) = &message.animation {
        Some(SentAs::Animation(animation.file_id.clone()))
    } else if let Some(document) = &message.document {
        Some(SentAs::Document(document.file_id.clone()))
    } else {
        None
    }
}

async fn cache_video(
    redis: &redis::aio::ConnectionManager,
    display_name: &str,
    sent_as: &SentAs,
) -> Result<(), Error> {
    let mut redis = redis.clone();

    let data = serde_json::to_vec(sent_as)?;
    redis
        .set_ex(format!("video:{}", display_name), data, 60 * 60 * 24)
        .await?;

    Ok(())
}

async fn send_previous_video(
    cx: &Context,
    sent_as: &SentAs,
    chat_id: ChatID,
    reply_markup: tgbotapi::requests::ReplyMarkup,
    height: i32,
    width: i32,
    duration: i32,
) -> Result<(), tgbotapi::Error> {
    let resp = match sent_as {
        SentAs::Video(ref file_id) => {
            let send_video = SendVideo {
                chat_id,
                video: FileType::FileID(file_id.clone()),
                reply_markup: Some(reply_markup.clone()),
                supports_streaming: Some(true),
                height: Some(height),
                width: Some(width),
                duration: Some(duration),
                ..Default::default()
            };

            cx.make_request(&send_video).await
        }
        SentAs::Animation(ref file_id) => {
            let send_animation = SendAnimation {
                chat_id,
                animation: FileType::FileID(file_id.clone()),
                reply_markup: Some(reply_markup.clone()),
                height: Some(height),
                width: Some(width),
                duration: Some(duration),
                ..Default::default()
            };

            cx.make_request(&send_animation).await
        }
        SentAs::Document(ref file_id) => {
            let send_document = SendDocument {
                chat_id,
                document: FileType::FileID(file_id.clone()),
                reply_markup: Some(reply_markup.clone()),
                ..Default::default()
            };

            cx.make_request(&send_document).await
        }
    };

    resp.map(|_message| ())
}

async fn get_cached_video(
    redis: &redis::aio::ConnectionManager,
    display_name: &str,
) -> Result<Option<SentAs>, Error> {
    let mut redis = redis.clone();

    let data: Option<Vec<u8>> = redis.get(format!("video:{}", display_name)).await?;

    Ok(data.and_then(|data| serde_json::from_slice(&data).ok()))
}
