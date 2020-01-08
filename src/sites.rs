use async_trait::async_trait;
use reqwest::header;
use serde::Deserialize;
use std::collections::HashMap;
use tokio01::runtime::current_thread::block_on_all;

const USER_AGENT: &str = concat!(
    "t.me/FoxBot version ",
    env!("CARGO_PKG_VERSION"),
    " developed by @Syfaro"
);

#[derive(Debug)]
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
}

fn get_file_ext(name: &str) -> Option<&str> {
    name.split('.').last()
}

#[derive(Debug)]
pub struct SiteError {
    error: Option<Box<dyn std::error::Error>>,
}

impl From<reqwest::Error> for SiteError {
    fn from(error: reqwest::Error) -> SiteError {
        SiteError {
            error: Some(Box::new(error)),
        }
    }
}

impl From<serde_json::Error> for SiteError {
    fn from(error: serde_json::Error) -> SiteError {
        SiteError {
            error: Some(Box::new(error)),
        }
    }
}

impl From<std::option::NoneError> for SiteError {
    fn from(_: std::option::NoneError) -> SiteError {
        SiteError { error: None }
    }
}

impl From<egg_mode::error::Error> for SiteError {
    fn from(error: egg_mode::error::Error) -> SiteError {
        SiteError {
            error: Some(Box::new(error)),
        }
    }
}

#[async_trait]
pub trait Site {
    fn name(&self) -> &'static str;
    async fn url_supported(&mut self, url: &str) -> bool;
    async fn get_images(
        &mut self,
        user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError>;
}

pub struct Direct {
    client: reqwest::Client,
    fautil: std::sync::Arc<fautil::FAUtil>,
}

impl Direct {
    const EXTENSIONS: &'static [&'static str] = &["png", "jpg", "jpeg", "gif"];
    const TYPES: &'static [&'static str] = &["image/png", "image/jpeg", "image/gif"];

    pub fn new(fautil: std::sync::Arc<fautil::FAUtil>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("Unable to create client");

        Self { fautil, client }
    }

    async fn reverse_search(&self, url: &str) -> Option<fautil::ImageLookup> {
        let image = self.client.get(url).send().await;

        let image = match image {
            Ok(res) => res.bytes().await,
            Err(_) => return None,
        };

        let body = match image {
            Ok(body) => body,
            Err(_) => return None,
        };

        let results = self.fautil.image_search(body.to_vec(), true).await;

        match results {
            Ok(results) => results.into_iter().next(),
            Err(_) => None,
        }
    }
}

#[async_trait]
impl Site for Direct {
    fn name(&self) -> &'static str {
        "direct links"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        // If the URL extension isn't one in our list, ignore.
        if !Direct::EXTENSIONS.iter().any(|ext| url.ends_with(ext)) {
            return false;
        }

        // Make a HTTP HEAD request to determine the Content-Type.
        let resp = match self
            .client
            .head(url)
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => return false,
        };

        if !resp.status().is_success() {
            return false;
        }

        let content_type = match resp.headers().get(reqwest::header::CONTENT_TYPE) {
            Some(content_type) => content_type,
            None => return false,
        };

        // Return if the Content-Type is in our list.
        Direct::TYPES.iter().any(|t| content_type == t)
    }

    async fn get_images(
        &mut self,
        _user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let u = url.to_string();
        let mut source_link = None;

        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_secs(4), self.reverse_search(&u)).await
        {
            log::trace!("Got result from reverse search");
            if let Some(post) = result {
                log::debug!("Found ID of post matching: {}", post.id);
                source_link = Some(format!("https://www.furaffinity.net/view/{}/", post.id));
            } else {
                log::trace!("No posts matched");
            }
        } else {
            log::debug!("Reverse search timed out");
        }

        Ok(Some(vec![PostInfo {
            file_type: get_file_ext(url).unwrap().to_string(),
            url: u.clone(),
            thumb: None,
            source_link,
            extra_caption: None,
            personal: false,
        }]))
    }
}

pub struct E621 {
    show: regex::Regex,
    data: regex::Regex,

    client: reqwest::Client,
}

#[derive(Deserialize)]
struct E621Post {
    id: i32,
    file_url: String,
    preview_url: String,
    file_ext: String,
}

impl E621 {
    pub fn new() -> Self {
        Self {
            show: regex::Regex::new(r"https?://(?P<host>e(?:621|926)\.net)/post/show/(?P<id>\d+)(?:/(?P<tags>.+))?").unwrap(),
            data: regex::Regex::new(r"https?://(?P<host>static\d+\.e(?:621|926)\.net)/data/(?:(?P<modifier>sample|preview)/)?[0-9a-f]{2}/[0-9a-f]{2}/(?P<md5>[0-9a-f]{32})\.(?P<ext>.+)").unwrap(),

            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Site for E621 {
    fn name(&self) -> &'static str {
        "e621"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.show.is_match(url) || self.data.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let endpoint = if self.show.is_match(url) {
            let captures = self.show.captures(url).unwrap();
            let id = &captures["id"];

            format!("https://e621.net/post/show.json?id={}", id)
        } else {
            let captures = self.data.captures(url).unwrap();
            let md5 = &captures["md5"];

            format!("https://e621.net/post/show.json?md5={}", md5)
        };

        let resp: E621Post = self
            .client
            .get(&endpoint)
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await?
            .json()
            .await?;

        Ok(Some(vec![PostInfo {
            file_type: resp.file_ext,
            url: resp.file_url,
            thumb: Some(resp.preview_url),
            source_link: Some(format!("https://e621.net/post/show/{}", resp.id)),
            extra_caption: None,
            personal: false,
        }]))
    }
}

pub struct Twitter {
    matcher: regex::Regex,
    consumer: egg_mode::KeyPair,
    token: egg_mode::Token,
}

impl Twitter {
    pub fn new(consumer_key: String, consumer_secret: String) -> Self {
        use egg_mode::KeyPair;

        let consumer = KeyPair::new(consumer_key, consumer_secret);
        let token = block_on_all(egg_mode::bearer_token(&consumer)).unwrap();

        Self {
            matcher: regex::Regex::new(
                r"https://(?:mobile\.)?twitter.com/(?:\w+)/status/(?P<id>\d+)",
            )
            .unwrap(),
            consumer,
            token,
        }
    }
}

#[async_trait]
impl Site for Twitter {
    fn name(&self) -> &'static str {
        "Twitter"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(
        &mut self,
        user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};

        let captures = self.matcher.captures(url).unwrap();
        let id = captures["id"].to_owned().parse::<u64>().unwrap();

        log::trace!(
            "Attempting to find saved credentials for {}",
            &format!("credentials:{}", user_id)
        );

        let db_path = std::env::var("TWITTER_DATABASE").expect("Missing Twitter database path");
        let db = PickleDb::load(
            db_path.clone(),
            PickleDbDumpPolicy::AutoDump,
            SerializationMethod::Json,
        )
        .unwrap_or_else(|_| {
            PickleDb::new(
                db_path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
        });

        let saved: Option<(String, String)> = db.get(&format!("credentials:{}", user_id));
        log::debug!("User saved Twitter credentials: {:?}", saved);
        let token = match saved {
            Some(token) => egg_mode::Token::Access {
                consumer: self.consumer.clone(),
                access: egg_mode::KeyPair::new(token.0, token.1),
            },
            _ => self.token.clone(),
        };

        log::debug!("token: {:?}", token);

        let tweet = match block_on_all(egg_mode::tweet::show(id, &token)) {
            Ok(tweet) => tweet.response,
            Err(e) => return Err(e.into()),
        };

        let user = tweet.user.unwrap();

        let screen_name = user.screen_name.to_owned();
        let tweet_id = tweet.id;

        let media = match tweet.extended_entities {
            Some(entity) => entity.media,
            None => return Ok(None),
        };

        let link = format!("https://twitter.com/{}/status/{}", screen_name, tweet_id);

        Ok(Some(
            media
                .into_iter()
                .map(|item| PostInfo {
                    file_type: get_file_ext(&item.media_url_https).unwrap().to_owned(),
                    url: item.media_url_https.clone(),
                    thumb: Some(format!("{}:thumb", item.media_url_https.clone())),
                    source_link: Some(link.clone()),
                    extra_caption: None,
                    personal: user.protected,
                })
                .collect(),
        ))
    }
}

pub struct FurAffinity {
    cookies: (String, String),
    fapi: fautil::FAUtil,
    submission: scraper::Selector,
    client: reqwest::Client,
}

impl FurAffinity {
    pub fn new(cookies: (String, String), util_api: String) -> Self {
        Self {
            cookies,
            fapi: fautil::FAUtil::new(util_api),
            submission: scraper::Selector::parse("#submissionImg").unwrap(),
            client: reqwest::Client::new(),
        }
    }

    async fn load_direct_url(&self, url: &str) -> Result<Option<PostInfo>, SiteError> {
        let url = if url.starts_with("http://") {
            url.replace("http://", "https://")
        } else {
            url.to_string()
        };

        let sub: fautil::Lookup = match self.fapi.lookup_url(&url).await {
            Ok(mut results) if !results.is_empty() => results.remove(0),
            _ => {
                return Ok(Some(PostInfo {
                    file_type: get_file_ext(&url).unwrap().to_string(),
                    url: url.clone(),
                    thumb: None,
                    source_link: None,
                    extra_caption: None,
                    personal: false,
                }));
            }
        };

        Ok(Some(PostInfo {
            file_type: get_file_ext(&sub.filename).unwrap().to_string(),
            url: sub.url.clone(),
            thumb: None,
            source_link: Some(format!("https://www.furaffinity.net/view/{}/", sub.id)),
            extra_caption: None,
            personal: false,
        }))
    }

    async fn load_submission(&self, url: &str) -> Result<Option<PostInfo>, SiteError> {
        let cookies = vec![
            format!("a={}", self.cookies.0),
            format!("b={}", self.cookies.1),
        ]
        .join("; ");

        let resp = self
            .client
            .get(url)
            .header(header::COOKIE, cookies)
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await?
            .text()
            .await?;

        let body = scraper::Html::parse_document(&resp);
        let img = match body.select(&self.submission).next() {
            Some(img) => img,
            None => return Ok(None),
        };

        let image_url = format!("https:{}", img.value().attr("src")?);

        Ok(Some(PostInfo {
            file_type: get_file_ext(&image_url).unwrap().to_string(),
            url: image_url.clone(),
            thumb: None,
            source_link: Some(url.to_string()),
            extra_caption: None,
            personal: false,
        }))
    }
}

#[async_trait]
impl Site for FurAffinity {
    fn name(&self) -> &'static str {
        "FurAffinity"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        url.contains("furaffinity.net/view/")
            || url.contains("furaffinity.net/full/")
            || url.contains("facdn.net/art/")
    }

    async fn get_images(
        &mut self,
        _user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let image = if url.contains("facdn.net/art/") {
            self.load_direct_url(url).await
        } else {
            self.load_submission(url).await
        };

        image.map(|sub| sub.map(|post| vec![post]))
    }
}

pub struct Mastodon {
    instance_cache: HashMap<String, bool>,
    matcher: regex::Regex,
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
    pub fn new() -> Self {
        Self {
            instance_cache: HashMap::new(),
            matcher: regex::Regex::new(
                r#"(?P<host>https?://(?:\S+))/(?:notice|users/\w+/statuses|@\w+)/(?P<id>\d+)"#,
            )
            .unwrap(),
        }
    }
}

#[async_trait]
impl Site for Mastodon {
    fn name(&self) -> &'static str {
        "Mastodon"
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

        let resp = match reqwest::Client::new()
            .head(&format!("{}/api/v1/instance", base))
            .header(header::USER_AGENT, USER_AGENT)
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
        _user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let captures = self.matcher.captures(url).unwrap();

        let base = captures["host"].to_owned();
        let status_id = captures["id"].to_owned();

        let resp = reqwest::Client::new()
            .get(&format!("{}/api/v1/statuses/{}", base, status_id))
            .header(header::USER_AGENT, USER_AGENT)
            .send();

        let resp = match resp.await {
            Ok(resp) => resp,
            Err(e) => {
                return Err(SiteError {
                    error: Some(Box::new(e)),
                })
            }
        };

        let json: MastodonStatus = match resp.json().await {
            Ok(json) => json,
            Err(e) => {
                return Err(SiteError {
                    error: Some(Box::new(e)),
                })
            }
        };

        if json.media_attachments.is_empty() {
            return Ok(None);
        }

        Ok(Some(
            json.media_attachments
                .iter()
                .map(|media| PostInfo {
                    file_type: get_file_ext(&media.url).unwrap().to_owned(),
                    url: media.url.clone(),
                    thumb: Some(media.preview_url.clone()),
                    source_link: Some(json.url.clone()),
                    extra_caption: None,
                    personal: false,
                })
                .collect(),
        ))
    }
}

pub struct Weasyl {
    api_key: String,
    matcher: regex::Regex,
}

impl Weasyl {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            matcher: regex::Regex::new(r#"https?://www\.weasyl\.com/(?:(?:~|%7)(?:\w+)/submissions|submission)/(?P<id>\d+)(?:/\S+)"#).unwrap(),
        }
    }
}

#[async_trait]
impl Site for Weasyl {
    fn name(&self) -> &'static str {
        "Weasyl"
    }

    async fn url_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    async fn get_images(
        &mut self,
        _user_id: i32,
        url: &str,
    ) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let captures = self.matcher.captures(url).unwrap();
        let sub_id = captures["id"].to_owned();

        let resp: serde_json::Value = reqwest::Client::new()
            .get(&format!(
                "https://www.weasyl.com/api/submissions/{}/view",
                sub_id
            ))
            .header("X-Weasyl-API-Key", self.api_key.as_bytes())
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await?
            .json()
            .await?;

        let submissions = resp
            .as_object()?
            .get("media")?
            .as_object()?
            .get("submission")?
            .as_array()?;

        if submissions.is_empty() {
            return Ok(None);
        }

        let thumbs = resp
            .as_object()?
            .get("media")?
            .as_object()?
            .get("thumbnail")?
            .as_array()?;

        Ok(Some(
            submissions
                .iter()
                .zip(thumbs)
                .map(|(sub, thumb)| {
                    let sub_url = sub.get("url").unwrap().as_str().unwrap().to_owned();
                    let thumb_url = thumb.get("url").unwrap().as_str().unwrap().to_owned();

                    PostInfo {
                        file_type: get_file_ext(&sub_url).unwrap().to_owned(),
                        url: sub_url.clone(),
                        thumb: Some(thumb_url),
                        source_link: Some(url.to_string()),
                        extra_caption: None,
                        personal: false,
                    }
                })
                .collect(),
        ))
    }
}
