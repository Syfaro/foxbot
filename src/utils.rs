use linkify::Link;
use sentry::integrations::failure::capture_fail;
use std::time::Instant;

use crate::BoxedSite;

pub struct SiteCallback<'a> {
    pub site: &'a BoxedSite,
    pub link: &'a str,
    pub duration: i64,
    pub results: Vec<crate::PostInfo>,
}

pub async fn find_images<'a, C>(
    user: &telegram::User,
    links: Vec<Link<'a>>,
    sites: &mut [BoxedSite],
    callback: &mut C,
) -> Vec<&'a str>
where
    C: FnMut(SiteCallback),
{
    let mut missing = vec![];

    'link: for link in links {
        let link_str = link.as_str();

        for site in sites.iter_mut() {
            let start = Instant::now();

            if site.url_supported(link_str).await {
                log::debug!("Link {} supported by {}", link_str, site.name());

                match site.get_images(user.id, link_str).await {
                    // Got results successfully and there were results
                    // Execute callback with site info and results
                    Ok(results) if results.is_some() => {
                        let results = results.unwrap(); // we know this is safe
                        log::debug!("Found images: {:?}", results);
                        callback(SiteCallback {
                            site: &site,
                            link: link_str,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                    }
                    // Got results successfully and there were NO results
                    // Continue onto next link
                    Ok(_) => {
                        missing.push(link_str);
                        continue 'link;
                    }
                    // Got error while processing, report and move onto next link
                    Err(e) => {
                        missing.push(link_str);
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
    bundle: &fluent::FluentBundle<fluent::FluentResource>,
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
