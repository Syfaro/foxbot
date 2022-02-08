use async_trait::async_trait;
use fuzzysearch::MatchType;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap};

use crate::{
    models::{self, User},
    utils, Error,
};

/// User agent used with all HTTP requests to sites.
const USER_AGENT: &str = concat!(
    "t.me/FoxBot Site Loader Version ",
    env!("CARGO_PKG_VERSION"),
    " developed by @Syfaro"
);

/// A thread-safe and boxed Site.
pub type BoxedSite = Box<dyn Site + Send + Sync>;

/// A collection of information about a post obtained from a given URL.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PostInfo {
    /// File type, as a standard file extension (png, jpg, etc.)
    pub file_type: String,
    /// URL to full image
    pub url: String,
    /// If this result is personal
    pub personal: bool,
    /// URL to thumbnail, if available
    pub thumb: Option<String>,
    /// URL to original source of this image, if available
    pub source_link: Option<String>,
    /// Additional caption to add as a second result for the provided query
    pub extra_caption: Option<String>,
    /// Title for video results
    pub title: Option<String>,
    /// Human readable name of the site
    pub site_name: Cow<'static, str>,
    /// Width and height of image, if available
    pub image_dimensions: Option<(u32, u32)>,
    /// Size of image in bytes, if available
    pub image_size: Option<usize>,

    pub artist_username: Option<String>,
    pub artist_url: Option<String>,
    pub submission_title: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// A basic attempt to get the extension from a given URL. It assumes the URL
/// ends in a filename with an extension.
fn get_file_ext(name: &str) -> Option<&str> {
    name.split('.').last().and_then(|ext| ext.split('?').next())
}

/// A site that we can potentially load image data from.
#[async_trait]
pub trait Site {
    /// The name of the site, as might be displayed to a user.
    fn name(&self) -> &'static str;
    /// A unique ID deterministically generated from the URL.
    fn url_id(&self, url: &str) -> Option<String>;

    /// Check if the URL might be supported by this site.
    async fn url_supported(&mut self, url: &str) -> bool;
    /// Attempt to load images from the given URL, with the Telegram user ID
    /// in case credentials are needed.
    async fn get_images(&mut self, user: &User, url: &str) -> Result<Option<Vec<PostInfo>>, Error>;
}

pub async fn get_all_sites(
    config: &crate::RunConfig,
    pool: sqlx::Pool<sqlx::Postgres>,
) -> Vec<BoxedSite> {
    vec![
        Box::new(E621::new(
            E621Host::E621,
            config.e621_login.clone(),
            config.e621_api_token.clone(),
        )),
        Box::new(E621::new(
            E621Host::E926,
            config.e621_login.clone(),
            config.e621_api_token.clone(),
        )),
        Box::new(FurAffinity::new(
            (
                config.furaffinity_cookie_a.clone(),
                config.furaffinity_cookie_b.clone(),
            ),
            config.fuzzysearch_api_token.clone(),
        )),
        Box::new(Weasyl::new(config.weasyl_api_token.clone())),
        Box::new(
            Twitter::new(
                config.twitter_consumer_key.clone(),
                config.twitter_consumer_secret.clone(),
                pool,
            )
            .await,
        ),
        Box::new(Inkbunny::new(
            config.inkbunny_username.clone(),
            config.inkbunny_password.clone(),
        )),
        Box::new(Mastodon::default()),
        Box::new(DeviantArt::default()),
        Box::new(Direct::new(config.fuzzysearch_api_token.clone())),
    ]
}

/// A loader for any direct image URL.
///
/// It attempts to check the image against FuzzySearch to determine if there is
/// a known source.
///
/// This should always be checked last as it will accept any URL with an image
/// extension, potentially blocking loaders that are more specific.
pub struct Direct {
    client: reqwest::Client,
    fautil: std::sync::Arc<fuzzysearch::FuzzySearch>,
}

impl Direct {
    pub fn new(fuzzysearch_apitoken: String) -> Self {
        let fautil = std::sync::Arc::new(fuzzysearch::FuzzySearch::new(fuzzysearch_apitoken));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .user_agent(USER_AGENT)
            .build()
            .expect("Unable to create client");

        Self { client, fautil }
    }

    /// Attempt to download the image from the given URL and search the contents
    /// against FuzzySearch. It uses a small distance to ensure it's a valid
    /// source and keep the request fast, but a timeout should be applied for
    /// use in inline queries in case FuzzySearch is running behind.
    async fn reverse_search(&self, url: &str) -> Option<fuzzysearch::File> {
        let mut checked_download = utils::CheckFileSize::new(url, 20_000_000);
        let bytes = checked_download.get_bytes().await;

        let body = match bytes {
            Ok(body) => body,
            Err(_) => return None,
        };

        let results = self
            .fautil
            .image_search(body, MatchType::Exact, Some(1))
            .await;

        match results {
            Ok(results) => results.matches.into_iter().next(),
            Err(_) => None,
        }
    }
}

#[async_trait]
impl Site for Direct {
    fn name(&self) -> &'static str {
        "direct link"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        Some(url.to_owned())
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        tracing::trace!("checking if url is supported");

        let mut data = match self.client.get(url).send().await {
            Ok(data) => data,
            Err(_err) => return false,
        };

        let mut buf = bytes::BytesMut::new();

        while let Ok(Some(chunk)) = data.chunk().await {
            if buf.len() + chunk.len() > 20_000_000 {
                tracing::warn!("buf wanted to be larger than max download size");
                return false;
            }

            buf.extend(chunk);

            if buf.len() > 8_192 {
                break;
            }
        }

        infer::is_image(&buf) || infer::is_video(&buf)
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let u = url.to_string();
        let mut source_link = None;
        let mut source_name = None;

        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_secs(4), self.reverse_search(&u)).await
        {
            tracing::trace!("got result from reverse search");
            if let Some(post) = result {
                tracing::debug!("found matching post");
                source_link = Some(post.url());
                source_name = Some(post.site_name());
            } else {
                tracing::trace!("no posts matched");
            }
        } else {
            tracing::warn!("reverse search timed out");
        }

        let ext = match get_file_ext(url) {
            Some(ext) => ext,
            None => return Ok(None),
        };

        Ok(Some(vec![PostInfo {
            file_type: ext.to_string(),
            url: u.clone(),
            source_link,
            site_name: source_name.unwrap_or_else(|| self.name()).into(),
            ..Default::default()
        }]))
    }
}

pub enum E621Host {
    E621,
    E926,
}

impl E621Host {
    pub fn name(&self) -> &'static str {
        match self {
            E621Host::E621 => "e621",
            E621Host::E926 => "e926",
        }
    }

    pub fn host(&self) -> &'static str {
        match self {
            E621Host::E621 => "e621.net",
            E621Host::E926 => "e926.net",
        }
    }
}

/// A loader for e621 posts and pools.
///
/// It can convert direct image links back into post URLs. It will only load the
/// 10 most recent posts when given a pool link.
pub struct E621 {
    show: regex::Regex,
    data: regex::Regex,
    pool: regex::Regex,

    client: reqwest::Client,

    site: E621Host,
    auth: (String, String),
}

#[derive(Debug, Deserialize)]
struct E621PostFile {
    ext: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct E621PostPreview {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct E621Post {
    id: i32,
    file: E621PostFile,
    preview: E621PostPreview,
    tags: HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct E621Resp {
    post: Option<E621Post>,
}

#[derive(Debug, Deserialize)]
struct E621Pool {
    id: i32,
    post_count: usize,
    post_ids: Vec<i32>,
}

struct E621Data {
    id: i32,
    file_url: String,
    file_ext: String,
    preview_url: String,
    artists: Vec<String>,
}

impl E621 {
    const INVALID_ARTISTS: &'static [&'static str] = &[
        "unknown_artist",
        "conditional_dnp",
        "anonymous_artist",
        "sound_warning",
    ];

    pub fn new(host: E621Host, login: String, api_key: String) -> Self {
        Self {
            show: regex::Regex::new(&format!(r"(?:https?://)?{}/(?:post/show/|posts/)(?P<id>\d+)(?:/(?P<tags>.+))?", host.host())).unwrap(),
            data: regex::Regex::new(&format!(r"(?:https?://)?(?:static\d+\.{})/data/(?:(?P<modifier>sample|preview)/)?[0-9a-f]{{2}}/[0-9a-f]{{2}}/(?P<md5>[0-9a-f]{{32}})\.(?P<ext>.+)", host.host())).unwrap(),
            pool: regex::Regex::new(&format!(r"(?:https?://)?{}/pools/(?P<id>\d+)(?:/(?P<tags>.+))?", host.host())).unwrap(),

            client: reqwest::Client::builder().user_agent(USER_AGENT).build().unwrap(),

            site: host,
            auth: (login, api_key),
        }
    }

    fn get_urls(resp: E621Resp) -> Option<E621Data> {
        match resp {
            E621Resp {
                post:
                    Some(E621Post {
                        id,
                        file:
                            E621PostFile {
                                ext: Some(file_ext),
                                url: Some(file_url),
                                ..
                            },
                        preview:
                            E621PostPreview {
                                url: Some(preview_url),
                            },
                        tags,
                        ..
                    }),
            } => Some(E621Data {
                id,
                file_url,
                file_ext,
                preview_url,
                artists: tags
                    .into_iter()
                    .filter(|(category, _tags)| category == "artist")
                    .flat_map(|(_category, tags)| tags)
                    .filter(|tag| !Self::INVALID_ARTISTS.contains(&&**tag))
                    .collect(),
            }),
            _ => None,
        }
    }

    /// Load the 10 most recent posts from a pool at a given URL.
    #[tracing::instrument(skip(self, url), fields(pool_id))]
    async fn get_pool(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self.pool.captures(url).unwrap();
        let id = &captures["id"];
        tracing::Span::current().record("pool_id", &id);

        tracing::trace!("Loading e621 pool");

        let endpoint = format!("https://{}/pools/{}.json", self.site.host(), id);
        let resp: E621Pool = self.load(&endpoint).await?;

        tracing::trace!(count = resp.post_count, "Discovered e621 pool items");

        let mut posts = Vec::with_capacity(resp.post_count);

        for post_id in resp.post_ids.iter().rev().take(10).rev() {
            tracing::trace!(post_id, "Loading e621 post as part of pool");

            let url = format!("https://{}/posts/{}.json", self.site.host(), post_id);
            let resp: E621Resp = self.load(&url).await?;

            let E621Data {
                id,
                file_url,
                file_ext,
                preview_url,
                artists,
            } = match Self::get_urls(resp) {
                Some(vals) => vals,
                None => continue,
            };

            posts.push(PostInfo {
                file_type: file_ext,
                url: file_url,
                thumb: Some(preview_url),
                source_link: Some(format!("https://{}/posts/{}", self.site.host(), id)),
                site_name: self.name().into(),
                artist_username: if !artists.is_empty() {
                    Some(artists.join(", "))
                } else {
                    None
                },
                ..Default::default()
            });
        }

        if posts.is_empty() {
            Ok(None)
        } else {
            Ok(Some(posts))
        }
    }

    /// Load arbitrary JSON data from a given URL.
    async fn load<T>(&self, url: &str) -> Result<T, Error>
    where
        T: serde::de::DeserializeOwned,
    {
        let resp = self
            .client
            .get(url)
            .basic_auth(&self.auth.0, Some(&self.auth.1))
            .send()
            .await
            .map_err(|err| Error::user_message_with_error("Could not connect to e621", err))?
            .json()
            .await
            .map_err(|err| Error::user_message_with_error("e621 returned unknown data", err))?;

        Ok(resp)
    }
}

#[async_trait]
impl Site for E621 {
    fn name(&self) -> &'static str {
        self.site.name()
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.show.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        let sub_id = &captures["id"];

        Some(format!("{}-{}", self.site.name(), sub_id))
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.show.is_match(url) || self.data.is_match(url) || self.pool.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let endpoint = if self.show.is_match(url) {
            let captures = self.show.captures(url).unwrap();
            let id = &captures["id"];

            format!("https://{}/posts/{}.json", self.site.host(), id)
        } else if self.data.is_match(url) {
            let captures = self.data.captures(url).unwrap();
            let md5 = &captures["md5"];

            format!("https://{}/posts.json?md5={}", self.site.host(), md5)
        } else {
            return self.get_pool(url).await;
        };

        let resp: E621Resp = self.load(&endpoint).await?;

        let E621Data {
            id,
            file_url,
            file_ext,
            preview_url,
            artists,
        } = match Self::get_urls(resp) {
            Some(vals) => vals,
            None => return Ok(None),
        };

        Ok(Some(vec![PostInfo {
            file_type: file_ext,
            url: file_url,
            thumb: Some(preview_url),
            source_link: Some(format!("https://{}/posts/{}", self.site.host(), id)),
            site_name: self.name().into(),
            artist_username: if !artists.is_empty() {
                Some(artists.join(", "))
            } else {
                None
            },
            ..Default::default()
        }]))
    }
}

/// A loader for Tweets.
///
/// It can use user credentials to get Tweets from locked accounts.
pub struct Twitter {
    matcher: regex::Regex,
    consumer: egg_mode::KeyPair,
    token: egg_mode::Token,
    conn: sqlx::Pool<sqlx::Postgres>,
}

impl Twitter {
    fn update_error(err: egg_mode::error::Error) -> Error {
        match &err {
            egg_mode::error::Error::TwitterError(_headers, twitter_errors) => {
                let codes: std::collections::HashSet<i32> = twitter_errors
                    .errors
                    .iter()
                    .map(|error| error.code)
                    .collect();

                if codes.contains(&34)
                    || codes.contains(&144)
                    || codes.contains(&421)
                    || codes.contains(&422)
                {
                    return Error::user_message_with_error("Tweet not found", err);
                } else if codes.contains(&50) || codes.contains(&63) {
                    return Error::user_message_with_error("Twitter user not found", err);
                } else if codes.contains(&179) {
                    return Error::user_message_with_error("Tweet is from locked account", err);
                }
            }
            err => tracing::error!("got unknown twitter error: {:?}", err),
        }

        Error::user_message_with_error("Twitter returned unknown data", err)
    }
}

struct TwitterData {
    user: Box<egg_mode::user::TwitterUser>,
    media: Vec<egg_mode::entities::MediaEntity>,
    hashtags: Option<Vec<String>>,
}

impl Twitter {
    pub async fn new(
        consumer_key: String,
        consumer_secret: String,
        conn: sqlx::Pool<sqlx::Postgres>,
    ) -> Self {
        use egg_mode::KeyPair;

        let consumer = KeyPair::new(consumer_key, consumer_secret);
        let token = egg_mode::auth::bearer_token(&consumer).await.unwrap();

        Self {
            matcher: regex::Regex::new(
                r"https://(?:mobile\.)?twitter.com/(?P<screen_name>\w+)(?:/status/(?P<id>\d+))?",
            )
            .unwrap(),
            consumer,
            token,
            conn,
        }
    }

    /// Get the media from a captured URL. If it is a direct link to a tweet,
    /// attempt to load images from it. Otherwise, get the user's most recent
    /// media.
    async fn get_media(
        &self,
        token: &egg_mode::Token,
        captures: &regex::Captures<'_>,
    ) -> Result<Option<TwitterData>, Error> {
        if let Some(Ok(id)) = captures.name("id").map(|id| id.as_str().parse::<u64>()) {
            let tweet = egg_mode::tweet::show(id, token)
                .await
                .map_err(Self::update_error)?
                .response;

            let user = match tweet.user {
                Some(user) => user,
                None => return Ok(None),
            };
            let media = match tweet.extended_entities {
                Some(entities) => entities.media,
                None => return Ok(None),
            };

            let hashtags = tweet
                .entities
                .hashtags
                .iter()
                .map(|hashtag| hashtag.text.clone())
                .collect();

            Ok(Some(TwitterData {
                user,
                media,
                hashtags: Some(hashtags),
            }))
        } else {
            let user = captures["screen_name"].to_owned();
            let timeline =
                egg_mode::tweet::user_timeline(user, false, false, token).with_page_size(200);
            let (_timeline, feed) = timeline.start().await.map_err(Self::update_error)?;

            let first_tweet = match feed.iter().next() {
                Some(tweet) => tweet,
                None => return Ok(None),
            };
            let user = match &first_tweet.user {
                Some(user) => Box::new(user.as_ref().to_owned()),
                None => return Ok(None),
            };

            let media = feed
                .into_iter()
                .filter_map(|tweet| Some(tweet.extended_entities.as_ref()?.media.clone()))
                .take(5) // Only take from 5 most recent media tweets
                .flatten()
                .collect();

            Ok(Some(TwitterData {
                user,
                media,
                hashtags: None,
            }))
        }
    }
}

#[async_trait]
impl Site for Twitter {
    fn name(&self) -> &'static str {
        "Twitter"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        // Get the ID of the Tweet if possible, otherwise use the screen name.
        let id = captures
            .name("id")
            .map(|id| id.as_str())
            .unwrap_or(&captures["screen_name"]);

        Some(format!("Twitter-{}", id))
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(&mut self, user: &User, url: &str) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self.matcher.captures(url).unwrap();

        tracing::trace!(%user, "attempting to find saved credentials",);

        let account = models::TwitterAccount::get(&self.conn, user).await?;

        let token = match account {
            Some(account) => egg_mode::Token::Access {
                consumer: self.consumer.clone(),
                access: egg_mode::KeyPair::new(account.consumer_key, account.consumer_secret),
            },
            _ => self.token.clone(),
        };

        let TwitterData {
            user,
            media,
            hashtags,
        } = match self.get_media(&token, &captures).await? {
            None => return Ok(None),
            Some(data) => data,
        };

        Ok(Some(
            media
                .into_iter()
                .filter_map(|item| match get_best_video(&item) {
                    Some(video_url) => Some(PostInfo {
                        file_type: get_file_ext(video_url)?.to_owned(),
                        url: video_url.to_string(),
                        thumb: Some(format!("{}:thumb", item.media_url_https.clone())),
                        source_link: Some(item.expanded_url),
                        personal: user.protected,
                        title: Some(user.screen_name.clone()),
                        site_name: self.name().into(),
                        tags: hashtags.clone(),
                        artist_username: Some(user.screen_name.clone()),
                        artist_url: Some(format!("https://twitter.com/{}", user.screen_name)),
                        ..Default::default()
                    }),
                    None => Some(PostInfo {
                        file_type: get_file_ext(&item.media_url_https)?.to_owned(),
                        url: item.media_url_https.clone(),
                        thumb: Some(format!("{}:thumb", item.media_url_https.clone())),
                        source_link: Some(item.expanded_url),
                        personal: user.protected,
                        site_name: self.name().into(),
                        tags: hashtags.clone(),
                        artist_username: Some(user.screen_name.clone()),
                        artist_url: Some(format!("https://twitter.com/{}", user.screen_name)),
                        ..Default::default()
                    }),
                })
                .collect(),
        ))
    }
}

/// Find the video in a Tweet with the highest bitrate.
fn get_best_video(media: &egg_mode::entities::MediaEntity) -> Option<&str> {
    let video_info = match &media.video_info {
        Some(video_info) => video_info,
        None => return None,
    };

    let highest_bitrate = video_info
        .variants
        .iter()
        .max_by_key(|video| video.bitrate.unwrap_or(0))?;

    Some(&highest_bitrate.url)
}

/// A loader for FurAffinity.
///
/// It converts direct image URLs back into submission URLs using FuzzySearch.
pub struct FurAffinity {
    fapi: fuzzysearch::FuzzySearch,
    matcher: regex::Regex,
    fa: furaffinity_rs::FurAffinity,
}

impl FurAffinity {
    pub fn new(cookies: (String, String), util_api: String) -> Self {
        let fa =
            furaffinity_rs::FurAffinity::new(cookies.0, cookies.1, USER_AGENT.to_string(), None);

        Self {
            fapi: fuzzysearch::FuzzySearch::new(util_api),
            fa,
            matcher: regex::Regex::new(
                r#"(?:https?://)?(?:(?:www\.)?furaffinity\.net/(?:view|full)/(?P<id>\d+)/?|(?:d\.furaffinity\.net|d\.facdn\.net)/art/\w+/(?P<file_id>\d+)/(?P<file_name>\S+))"#,
            )
            .unwrap(),
        }
    }

    /// Attempt to resolve a direct image URL into a submission using
    /// FuzzySearch.
    async fn load_direct_url(&self, filename: &str, url: &str) -> Result<Option<PostInfo>, Error> {
        let sub: fuzzysearch::File = match self.fapi.lookup_filename(filename).await {
            Ok(mut results) if !results.is_empty() => results.remove(0),
            _ => {
                let ext = match get_file_ext(url) {
                    Some(ext) => ext,
                    None => return Ok(None),
                };

                return Ok(Some(PostInfo {
                    file_type: ext.to_string(),
                    url: url.to_owned(),
                    site_name: self.name().into(),
                    ..Default::default()
                }));
            }
        };

        let ext = match get_file_ext(&sub.filename) {
            Some(ext) => ext,
            None => return Ok(None),
        };

        Ok(Some(PostInfo {
            file_type: ext.to_string(),
            url: sub.url.clone(),
            source_link: Some(sub.url()),
            site_name: self.name().into(),
            ..Default::default()
        }))
    }

    async fn load_from_fa(&self, id: i32, url: &str) -> Result<Option<Vec<PostInfo>>, Error> {
        let sub = self.fa.get_submission(id).await.map_err(|err| {
            Error::user_message_with_error("Unable to get FurAffinity submission", err)
        })?;
        let sub = match sub {
            Some(sub) => sub,
            None => return Ok(None),
        };

        let image_url = match &sub.content {
            furaffinity_rs::Content::Image(image) => image,
            _ => return Ok(None),
        };

        let ext = match get_file_ext(image_url) {
            Some(ext) => ext,
            None => return Ok(None),
        };

        let fallback = vec![PostInfo {
            file_type: ext.to_string(),
            url: image_url.clone(),
            source_link: Some(url.to_string()),
            site_name: self.name().into(),
            title: Some(sub.title.clone()),
            artist_username: Some(sub.artist.clone()),
            artist_url: Some(format!("https://www.furaffinity.net/user/{}/", sub.artist)),
            tags: Some(sub.tags.clone()),
            ..Default::default()
        }];

        if let Some(nav_links) = sub.nav_links() {
            tracing::info!("submission had nav links, attempting to expand");

            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.collect_linked_posts(url, sub, nav_links),
            )
            .await
            {
                // Timeout, ignore and return fallback
                Err(_err) => Ok(Some(fallback)),
                // Error fetching more submissions, ignore and fallback
                Ok(Err(_err)) => Ok(Some(fallback)),
                // Got posts, use new posts
                Ok(Ok(posts)) => Ok(posts),
            }
        } else {
            Ok(Some(fallback))
        }
    }

    async fn collect_linked_posts(
        &self,
        url: &str,
        sub: furaffinity_rs::Submission,
        nav_links: furaffinity_rs::NavLinks,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let mut next_id = nav_links.first.unwrap_or(sub.id);
        let mut submissions = HashMap::new();

        loop {
            tracing::trace!(next_id, "loading next submission in navigation");

            if submissions.contains_key(&next_id) {
                tracing::warn!("trying to visit already visited id");
                break;
            }

            // If trying to load the root submission we were given, use the
            // value we previously fetched.
            let sub = if next_id == sub.id {
                sub.clone()
            } else {
                match self.fa.get_submission(next_id).await {
                    Ok(Some(sub)) => sub,
                    _ => break,
                }
            };

            let links = sub.nav_links();

            submissions.insert(sub.id, sub);

            match links {
                Some(furaffinity_rs::NavLinks {
                    next: Some(next), ..
                }) => next_id = next,
                _ => break,
            }
        }

        tracing::debug!("found {} images in collection", submissions.len());

        let mut posts: Vec<_> = submissions.into_iter().map(|(_id, sub)| sub).collect();
        // Posts probably belong in ID order
        posts.sort_by(|a, b| a.id.cmp(&b.id));

        if let Some(first) = posts.iter().position(|loaded_sub| sub.id == loaded_sub.id) {
            // Original post should always be first
            if first != 0 {
                tracing::debug!(
                    searched_item = sub.id,
                    current_pos = first,
                    "moving searched item to front"
                );

                let post = posts.remove(first);
                posts.insert(0, post);
            }
        }

        let posts = posts
            .into_iter()
            .filter_map(|sub| {
                let image_url = match &sub.content {
                    furaffinity_rs::Content::Image(image) => image.to_owned(),
                    _ => return None,
                };

                let ext = match get_file_ext(&image_url) {
                    Some(ext) => ext.to_owned(),
                    None => return None,
                };

                Some((sub, ext, image_url))
            })
            .map(|(sub, ext, image_url)| PostInfo {
                file_type: ext,
                url: image_url,
                source_link: Some(url.to_string()),
                site_name: self.name().into(),
                title: Some(sub.title),
                artist_username: Some(sub.artist.clone()),
                artist_url: Some(format!("https://www.furaffinity.net/user/{}/", sub.artist)),
                tags: Some(sub.tags),
                ..Default::default()
            })
            .collect();

        Ok(Some(posts))
    }
}

#[async_trait]
impl Site for FurAffinity {
    fn name(&self) -> &'static str {
        "FurAffinity"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        if let Some(sub_id) = captures.name("id") {
            Some(format!("FurAffinity-{}", sub_id.as_str()))
        } else {
            captures
                .name("file_id")
                .map(|file_id| format!("FurAffinityFile-{}", file_id.as_str()))
        }
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self
            .matcher
            .captures(url)
            .ok_or_else(|| Error::bot("supported url should always match"))?;

        if let Some(filename) = captures.name("file_name") {
            self.load_direct_url(filename.as_str(), url)
                .await
                .map(|sub| sub.map(|post| vec![post]))
        } else if let Some(id) = captures.name("id") {
            let id: i32 = match id.as_str().parse() {
                Ok(id) => id,
                Err(_err) => return Ok(None),
            };
            self.load_from_fa(id, url).await
        } else {
            return Ok(None);
        }
    }
}

/// A loader for Mastodon instances.
///
/// It holds an in-memory cache of if a URL is a Mastodon instance.
pub struct Mastodon {
    instance_cache: HashMap<String, bool>,
    matcher: regex::Regex,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct MastodonStatus {
    url: String,
    media_attachments: Vec<MastodonMediaAttachments>,
}

#[derive(Deserialize)]
struct MastodonMediaAttachments {
    url: String,
    preview_url: String,
}

impl Mastodon {
    pub fn default() -> Self {
        Self {
            instance_cache: HashMap::new(),
            matcher: regex::Regex::new(
                r#"(?P<host>https?://(?:\S+))/(?:notice|users/\w+/statuses|@\w+)/(?P<id>\d+)"#,
            )
            .unwrap(),
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .build()
                .unwrap(),
        }
    }
}

#[async_trait]
impl Site for Mastodon {
    fn name(&self) -> &'static str {
        "Mastodon"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        let sub_id = &captures["id"];

        Some(format!("Mastodon-{}", sub_id))
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            None => return false,
        };

        let base = captures["host"].to_owned();

        if let Some(is_masto) = self.instance_cache.get(&base) {
            if !is_masto {
                return false;
            }
        }

        let resp = match self
            .client
            .head(&format!("{}/api/v1/instance", base))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => {
                self.instance_cache.insert(base, false);
                return false;
            }
        };

        if !resp.status().is_success() {
            self.instance_cache.insert(base, false);
            return false;
        }

        true
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self.matcher.captures(url).unwrap();

        let base = captures["host"].to_owned();
        let status_id = captures["id"].to_owned();

        let json: MastodonStatus = self
            .client
            .get(&format!("{}/api/v1/statuses/{}", base, status_id))
            .send()
            .await
            .map_err(|err| {
                Error::user_message_with_error("Unable to connect to Mastodon instance", err)
            })?
            .json()
            .await
            .map_err(|err| {
                Error::user_message_with_error("Mastodon instance returned unknown data", err)
            })?;

        if json.media_attachments.is_empty() {
            return Ok(None);
        }

        Ok(Some(
            json.media_attachments
                .iter()
                .filter_map(|media| {
                    Some(PostInfo {
                        file_type: get_file_ext(&media.url)?.to_owned(),
                        url: media.url.clone(),
                        thumb: Some(media.preview_url.clone()),
                        source_link: Some(json.url.clone()),
                        site_name: self.name().into(),
                        ..Default::default()
                    })
                })
                .collect(),
        ))
    }
}

#[derive(serde::Deserialize, Debug)]
struct WeasylSubmission {
    title: String,
    owner: String,
    owner_login: String,
    tags: Vec<String>,
    media: WeasylMedia,
}

#[derive(serde::Deserialize, Debug)]
struct WeasylMedia {
    submission: Vec<WeasylMediaSubmission>,
    thumbnail: Vec<WeasylMediaThumbnail>,
}

#[derive(serde::Deserialize, Debug)]
struct WeasylMediaSubmission {
    url: String,
}

#[derive(serde::Deserialize, Debug)]
struct WeasylMediaThumbnail {
    url: String,
}

/// A loader for Weasyl.
pub struct Weasyl {
    api_key: String,
    matcher: regex::Regex,
    client: reqwest::Client,
}

impl Weasyl {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            matcher: regex::Regex::new(r#"https?://www\.weasyl\.com/(?:(?:(?:~|%7)(?:\w+)/submissions|submission)|view)/(?P<id>\d+)(?:/\S+)?"#).unwrap(),
            client: reqwest::Client::builder().user_agent(USER_AGENT).build().unwrap(),
        }
    }
}

#[async_trait]
impl Site for Weasyl {
    fn name(&self) -> &'static str {
        "Weasyl"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        let sub_id: i32 = match captures["id"].to_owned().parse() {
            Ok(id) => id,
            _ => return None,
        };

        Some(format!("Weasyl-{}", sub_id))
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self.matcher.captures(url).unwrap();
        let sub_id = captures["id"].to_owned();

        let resp: WeasylSubmission = self
            .client
            .get(&format!(
                "https://www.weasyl.com/api/submissions/{}/view",
                sub_id
            ))
            .header("X-Weasyl-API-Key", self.api_key.as_bytes())
            .send()
            .await
            .map_err(|err| Error::user_message_with_error("Unable to connect to Weasyl", err))?
            .json()
            .await
            .map_err(|err| Error::user_message_with_error("Weasyl returned unknown data", err))?;

        if resp.media.submission.is_empty() {
            return Ok(None);
        }

        let title = Some(resp.title.clone());
        let tags = Some(resp.tags.clone());
        let artist_username = Some(resp.owner.clone());
        let artist_url = Some(format!("https://www.weasyl.com/~{}", resp.owner_login));

        Ok(Some(
            resp.media
                .submission
                .into_iter()
                .zip(resp.media.thumbnail)
                .filter_map(|(sub, thumb)| {
                    let sub_url = sub.url;
                    let thumb_url = thumb.url;

                    Some(PostInfo {
                        file_type: get_file_ext(&sub_url)?.to_owned(),
                        url: sub_url,
                        thumb: Some(thumb_url),
                        source_link: Some(url.to_string()),
                        site_name: self.name().into(),
                        title: title.clone(),
                        tags: tags.clone(),
                        artist_username: artist_username.clone(),
                        artist_url: artist_url.clone(),
                        ..Default::default()
                    })
                })
                .collect(),
        ))
    }
}

/// A loader for Inkbunny.
pub struct Inkbunny {
    client: reqwest::Client,
    matcher: regex::Regex,

    username: String,
    password: String,

    sid: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct InkbunnyLogin {
    sid: String,
    user_id: i32,
    ratingsmask: String,
}

#[derive(Deserialize, Debug)]
pub struct InkbunnyFile {
    file_id: String,
    file_name: String,
    thumbnail_url_medium_noncustom: String,
    file_url_screen: String,
}

#[derive(Deserialize, Debug)]
pub struct InkbunnyKeyword {
    keyword_name: String,
}

#[derive(Deserialize, Debug)]
pub struct InkbunnySubmission {
    submission_id: String,
    username: String,
    title: String,
    files: Vec<InkbunnyFile>,
    keywords: Vec<InkbunnyKeyword>,
}

#[derive(Deserialize, Debug)]
pub struct InkbunnySubmissions {
    results_count: i32,
    submissions: Vec<InkbunnySubmission>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum InkbunnyResponse<T> {
    Error { error_code: i32 },
    Success(T),
}

impl Inkbunny {
    /// API endpoint for logging into the site.
    const API_LOGIN: &'static str = "https://inkbunny.net/api_login.php";
    /// API endpoint for loading a submission.
    const API_SUBMISSIONS: &'static str = "https://inkbunny.net/api_submissions.php";

    /// Log into Inkbunny, getting a session ID for future requests.
    pub async fn get_sid(&mut self) -> Result<String, Error> {
        if let Some(sid) = &self.sid {
            return Ok(sid.clone());
        }

        let resp: InkbunnyResponse<InkbunnyLogin> = self
            .client
            .post(Self::API_LOGIN)
            .form(&vec![
                ("username", &self.username),
                ("password", &self.password),
            ])
            .send()
            .await
            .map_err(|err| Error::user_message_with_error("Unable to connect to Inkbunny", err))?
            .json()
            .await
            .map_err(|err| Error::user_message_with_error("Inkbunny returned unknown data", err))?;

        let login = match resp {
            InkbunnyResponse::Success(login) => login,
            InkbunnyResponse::Error { error_code: 0 } => {
                return Err(Error::bot("Inkbunny username/password was incorrect"));
            }
            _ => return Err(Error::bot("Unhandled Inkbunny error code")),
        };

        if login.ratingsmask != "11111" {
            return Err(Error::bot("Inkbunny account was missing permissions"));
        }

        self.sid = Some(login.sid.clone());
        Ok(login.sid)
    }

    /// Load submissions from provided IDs.
    pub async fn get_submissions(&mut self, ids: &[i32]) -> Result<InkbunnySubmissions, Error> {
        let ids: String = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let submissions = loop {
            tracing::debug!(?ids, "loading Inkbunny submissions");
            let sid = self.get_sid().await?;

            let resp: InkbunnyResponse<InkbunnySubmissions> = self
                .client
                .post(Self::API_SUBMISSIONS)
                .form(&vec![("sid", &sid), ("submission_ids", &ids)])
                .send()
                .await
                .map_err(|err| {
                    Error::user_message_with_error("Unable to connect to Inkbunny", err)
                })?
                .json()
                .await
                .map_err(|err| {
                    Error::user_message_with_error("Inkbunny returned unknown data", err)
                })?;

            match resp {
                InkbunnyResponse::Success(submissions) => break submissions,
                InkbunnyResponse::Error { error_code: 2 } => {
                    tracing::info!("Inkbunny SID expired");
                    self.sid = None;
                    continue;
                }
                _ => return Err(Error::bot("Unhandled Inkbunny error code")),
            };
        };

        Ok(submissions)
    }

    pub fn new(username: String, password: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .unwrap();

        Self {
            client,
            matcher: regex::Regex::new(r#"https?://inkbunny.net/s/(?P<id>\d+)"#).unwrap(),

            username,
            password,

            sid: None,
        }
    }
}

#[async_trait]
impl Site for Inkbunny {
    fn name(&self) -> &'static str {
        "Inkbunny"
    }

    fn url_id(&self, url: &str) -> Option<String> {
        let captures = match self.matcher.captures(url) {
            Some(captures) => captures,
            _ => return None,
        };

        let sub_id: i32 = match captures["id"].to_owned().parse() {
            Ok(id) => id,
            _ => return None,
        };

        Some(format!("Inkbunny-{}", sub_id))
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let captures = self.matcher.captures(url).unwrap();
        let sub_id: i32 = match captures["id"].to_owned().parse() {
            Ok(id) => id,
            Err(_err) => return Ok(None),
        };

        let submissions = self.get_submissions(&[sub_id]).await?;

        let mut results = Vec::with_capacity(1);

        for submission in submissions.submissions {
            let tags: Vec<String> = submission
                .keywords
                .iter()
                .map(|kw| kw.keyword_name.clone())
                .collect();

            for file in submission.files {
                let ext = match get_file_ext(&file.file_url_screen) {
                    Some(ext) => ext,
                    None => continue,
                };

                results.push(PostInfo {
                    file_type: ext.to_owned(),
                    url: file.file_url_screen.clone(),
                    thumb: Some(file.thumbnail_url_medium_noncustom.clone()),
                    source_link: Some(url.to_owned()),
                    site_name: self.name().into(),
                    title: Some(submission.title.clone()),
                    tags: Some(tags.clone()),
                    artist_username: Some(submission.username.clone()),
                    artist_url: Some(format!("https://inkbunny.net/{}", submission.username)),
                    ..Default::default()
                });
            }
        }

        Ok(Some(results))
    }
}

/// A loader for DeviantArt.
pub struct DeviantArt {
    client: reqwest::Client,
    matcher: regex::Regex,
}

/// DeviantArt oEmbed responses can contain either integers or strings, so
/// we provide a wrapper type for serde to deserialize from.
#[derive(Clone, Copy, Debug)]
struct AlwaysNum(u32);

// This code is heavily based on the example from here:
// https://users.rust-lang.org/t/deserialize-a-number-that-may-be-inside-a-string-serde-json/27318/4
impl<'de> serde::Deserialize<'de> for AlwaysNum {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;

        impl<'de> serde::de::Visitor<'de> for NumVisitor {
            type Value = AlwaysNum;

            fn expecting(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                fmt.write_str("integer or string")
            }

            fn visit_u64<E: serde::de::Error>(self, val: u64) -> Result<Self::Value, E> {
                match u32::try_from(val) {
                    Ok(val) => Ok(AlwaysNum(val)),
                    Err(_) => Err(E::custom("invalid integer")),
                }
            }

            fn visit_str<E: serde::de::Error>(self, val: &str) -> Result<Self::Value, E> {
                match val.parse::<u32>() {
                    Ok(val) => self.visit_u32(val),
                    Err(_) => Err(E::custom("failed to parse integer")),
                }
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

#[derive(Deserialize)]
struct DeviantArtOEmbed {
    #[serde(rename = "type")]
    file_type: String,
    url: String,
    thumbnail_url: String,
    width: AlwaysNum,
    height: AlwaysNum,
    title: String,
    tags: Option<String>,
    author_name: String,
    author_url: String,
}

impl DeviantArt {
    pub fn default() -> Self {
        Self {
            client: reqwest::Client::builder().user_agent(USER_AGENT).build().unwrap(),
            matcher: regex::Regex::new(r#"(?:(?:deviantart\.com/(?:.+/)?art/.+-|fav\.me/)(?P<id>\d+)|sta\.sh/(?P<code>\w+))"#)
                .unwrap(),
        }
    }

    /// Attempt to get an ID from our matcher's captures.
    fn get_id(&self, captures: &regex::Captures) -> Option<String> {
        if let Some(id) = captures.name("id") {
            return Some(id.as_str().to_string());
        }

        if let Some(code) = captures.name("code") {
            return Some(code.as_str().to_string());
        }

        None
    }
}

#[async_trait]
impl Site for DeviantArt {
    fn name(&self) -> &'static str {
        "DeviantArt"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    fn url_id(&self, url: &str) -> Option<String> {
        self.matcher
            .captures(url)
            .and_then(|captures| self.get_id(&captures))
            .map(|id| format!("DeviantArt-{}", id))
    }

    async fn get_images(
        &mut self,
        _user: &User,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, Error> {
        let mut endpoint = url::Url::parse("https://backend.deviantart.com/oembed").unwrap();
        endpoint.query_pairs_mut().append_pair("url", url);

        let resp: DeviantArtOEmbed = self
            .client
            .get(endpoint)
            .send()
            .await
            .map_err(|err| Error::user_message_with_error("Unable to connect to DeviantArt", err))?
            .json()
            .await
            .map_err(|err| {
                Error::user_message_with_error("DeviantArt returned unknown data", err)
            })?;

        if resp.file_type != "photo" {
            return Ok(None);
        }

        Ok(Some(vec![PostInfo {
            file_type: "png".to_string(),
            url: resp.url,
            thumb: Some(resp.thumbnail_url),
            source_link: Some(url.to_owned()),
            site_name: self.name().into(),
            image_dimensions: Some((resp.width.0, resp.height.0)),
            artist_username: Some(resp.author_name),
            artist_url: Some(resp.author_url),
            title: Some(resp.title),
            tags: resp.tags.map(|tags| {
                tags.split(", ")
                    .map(std::string::ToString::to_string)
                    .collect()
            }),
            ..Default::default()
        }]))
    }
}
