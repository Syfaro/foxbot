use crate::generate_id;
use crate::sites::PostInfo;
use crate::utils::*;
use async_trait::async_trait;
use sentry::integrations::failure::capture_fail;
use telegram::*;

pub struct InlineHandler;

#[async_trait]
impl crate::Handler for InlineHandler {
    fn name(&self) -> &'static str {
        "inline"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: Update,
        _command: Option<Command>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let inline = match update.inline_query {
            Some(inline) => inline,
            None => return Ok(false),
        };

        let links: Vec<_> = handler.finder.links(&inline.query).collect();
        let mut results: Vec<PostInfo> = Vec::new();

        log::info!("Got query: {}", inline.query);
        log::debug!("Found links: {:?}", links);

        let influx = handler.influx.clone();
        // Lock sites in order to find which of these links are usable
        {
            let mut sites = handler.sites.lock().await;
            let links = links.iter().map(|link| link.as_str()).collect();
            find_images(&inline.from, links, &mut sites, &mut |info| {
                let influx = influx.clone();
                let duration = info.duration;
                let count = info.results.len();
                let name = info.site.name();

                // Log a point to InfluxDB with information about our inline query
                tokio::spawn(async move {
                    let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "inline")
                        .add_tag("site", name.replace(" ", "_"))
                        .add_field("count", count as i32)
                        .add_field("duration", duration);

                    influx.query(&point).await
                });

                results.extend(info.results);
            })
            .await;
        }

        // Find if any of our results were personal. If they were, we need to
        // prevent this response from being cached for others.
        let personal = results.iter().any(|result| result.personal);

        let mut responses: Vec<InlineQueryResult> = vec![];

        for result in results {
            if let Some(items) = process_result(&handler, &result, &inline.from).await {
                responses.extend(items);
            }
        }

        // If we had no responses but the query was not empty, there were likely links
        // that we were unable to convert. We need to display that the links had no results.
        if responses.is_empty() && !inline.query.is_empty() {
            let article = handler
                .get_fluent_bundle(inline.from.language_code.as_deref(), |bundle| {
                    InlineQueryResult::article(
                        generate_id(),
                        get_message(&bundle, "inline-no-results-title", None).unwrap(),
                        get_message(&bundle, "inline-no-results-body", None).unwrap(),
                    )
                })
                .await;

            responses.push(article);
        }

        let mut answer_inline = AnswerInlineQuery {
            inline_query_id: inline.id,
            results: responses,
            is_personal: Some(personal),
            ..Default::default()
        };

        // If the query was empty, display a help button to make it easy to get
        // started using the bot.
        if inline.query.is_empty() {
            answer_inline.switch_pm_text = Some("Help".to_string());
            answer_inline.switch_pm_parameter = Some("help".to_string());
        }

        if let Err(e) = handler.bot.make_request(&answer_inline).await {
            with_user_scope(Some(&inline.from), None, || {
                capture_fail(&e);
            });
            log::error!("Unable to respond to inline: {:?}", e);
        }

        Ok(true)
    }
}

/// Convert a [PostInfo] struct into an InlineQueryResult.
///
/// It adds an inline keyboard for the direct link and source if available.
async fn process_result(
    handler: &crate::MessageHandler,
    result: &PostInfo,
    from: &User,
) -> Option<Vec<InlineQueryResult>> {
    let (direct, source) = handler
        .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
            (
                get_message(&bundle, "inline-direct", None).unwrap(),
                get_message(&bundle, "inline-source", None).unwrap(),
            )
        })
        .await;

    let mut row = vec![InlineKeyboardButton {
        text: direct,
        url: Some(result.url.clone()),
        callback_data: None,
        ..Default::default()
    }];

    if let Some(source_link) = &result.source_link {
        row.push(InlineKeyboardButton {
            text: source,
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
        "png" | "jpeg" | "jpg" => Some(build_image_result(
            &result,
            thumb_url,
            &keyboard,
            handler.use_proxy,
        )),
        "gif" => Some(build_gif_result(
            &result,
            thumb_url,
            &keyboard,
            handler.use_proxy,
        )),
        other => {
            log::warn!("Got unusable type: {}", other);
            None
        }
    }
}

fn build_image_result(
    result: &crate::sites::PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
    use_proxy: bool,
) -> Vec<InlineQueryResult> {
    let (full_url, thumb_url) = if use_proxy {
        (
            format!("https://images.weserv.nl/?url={}&output=jpg", result.url),
            format!(
                "https://images.weserv.nl/?url={}&output=jpg&w=300",
                thumb_url
            ),
        )
    } else {
        (result.url.clone(), thumb_url)
    };

    let mut photo =
        InlineQueryResult::photo(generate_id(), full_url.to_owned(), thumb_url.to_owned());
    photo.reply_markup = Some(keyboard.clone());

    let mut results = vec![photo];

    if let Some(message) = &result.extra_caption {
        let mut photo = InlineQueryResult::photo(generate_id(), full_url, thumb_url);
        photo.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::Photo(ref mut result) = photo.content {
            result.caption = Some(message.to_string());
        }

        results.push(photo);
    };

    results
}

fn build_gif_result(
    result: &crate::sites::PostInfo,
    thumb_url: String,
    keyboard: &InlineKeyboardMarkup,
    use_proxy: bool,
) -> Vec<InlineQueryResult> {
    let (full_url, thumb_url) = if use_proxy {
        (
            format!("https://images.weserv.nl/?url={}&output=gif", result.url),
            format!(
                "https://images.weserv.nl/?url={}&output=gif&w=300",
                thumb_url
            ),
        )
    } else {
        (result.url.clone(), thumb_url)
    };

    let mut gif = InlineQueryResult::gif(generate_id(), full_url.to_owned(), thumb_url.to_owned());
    gif.reply_markup = Some(keyboard.clone());

    let mut results = vec![gif];

    if let Some(message) = &result.extra_caption {
        let mut gif = InlineQueryResult::gif(generate_id(), full_url, thumb_url);
        gif.reply_markup = Some(keyboard.clone());

        if let InlineQueryType::GIF(ref mut result) = gif.content {
            result.caption = Some(message.to_string());
        }

        results.push(gif);
    };

    results
}
