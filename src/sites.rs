use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug)]
pub struct PostInfo {
    pub file_type: String,
    pub url: String,
    pub thumb: String,
    pub caption: String,

    pub full_url: Option<String>,
    pub message: Option<String>,
}

fn get_file_ext(name: &str) -> Option<&str> {
    name.split('.').last()
}

#[derive(Debug, Clone)]
pub struct SiteError;

impl From<reqwest::Error> for SiteError {
    fn from(_: reqwest::Error) -> SiteError {
        SiteError {}
    }
}

impl From<serde_json::Error> for SiteError {
    fn from(_: serde_json::Error) -> SiteError {
        SiteError {}
    }
}

impl From<std::option::NoneError> for SiteError {
    fn from(_: std::option::NoneError) -> SiteError {
        SiteError {}
    }
}

pub trait Site {
    fn name(&self) -> &'static str;
    fn is_supported(&mut self, url: &str) -> bool;
    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError>;
}

pub struct Direct;

impl Direct {
    const EXTENSIONS: &'static [&'static str] = &["png", "jpg", "jpeg", "gif"];
    const TYPES: &'static [&'static str] = &["image/png", "image/jpeg", "image/gif"];

    pub fn new() -> Self {
        Self {}
    }
}

impl Site for Direct {
    fn name(&self) -> &'static str {
        "direct links"
    }

    fn is_supported(&mut self, url: &str) -> bool {
        // If the URL extension isn't one in our list, ignore.
        if !Direct::EXTENSIONS.iter().any(|ext| url.ends_with(ext)) {
            return false;
        }

        let client = reqwest::Client::new();

        // Make a HTTP HEAD request to determine the Content-Type.
        let resp = match client.head(url).send() {
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

    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let u = url.to_string();

        Ok(Some(vec![PostInfo {
            file_type: get_file_ext(url).unwrap().to_string(),
            url: u.clone(),
            thumb: u.clone(),
            caption: u,
            full_url: None,
            message: None,
        }]))
    }
}

pub struct e621 {
    show: regex::Regex,
    data: regex::Regex,

    client: reqwest::Client,
}

#[derive(Deserialize)]
struct e621Post {
    id: i32,
    file_url: String,
    preview_url: String,
    file_ext: String,
}

impl e621 {
    pub fn new() -> Self {
        Self {
            show: regex::Regex::new(r"https?://(?P<host>e(?:621|926)\.net)/post/show/(?P<id>\d+)(?:/(?P<tags>.+))?").unwrap(),
            data: regex::Regex::new(r"https?://(?P<host>static\d+\.e(?:621|926)\.net)/data/(?:(?P<modifier>sample|preview)/)?[0-9a-f]{2}/[0-9a-f]{2}/(?P<md5>[0-9a-f]{32})\.(?P<ext>.+)").unwrap(),

            client: reqwest::Client::new(),
        }
    }
}

impl Site for e621 {
    fn name(&self) -> &'static str {
        "e621"
    }

    fn is_supported(&mut self, url: &str) -> bool {
        self.show.is_match(url) || self.data.is_match(url)
    }

    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let endpoint = if self.show.is_match(url) {
            let captures = self.show.captures(url).unwrap();
            let id = &captures["id"];

            format!("https://e621.net/post/show.json?id={}", id).to_string()
        } else {
            let captures = self.data.captures(url).unwrap();
            let md5 = &captures["md5"];

            format!("https://e621.net/post/show.json?md5={}", md5).to_string()
        };

        let resp: e621Post = self.client.get(&endpoint).send()?.json()?;

        Ok(Some(vec![
            PostInfo {
                file_type: resp.file_ext,
                url: resp.file_url,
                thumb: resp.preview_url,
                caption: format!("https://e621.net/post/show/{}", resp.id),
                full_url: None,
                message: None,
            }
        ]))
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

    fn load_direct_url(&self, url: &str) -> Result<Option<PostInfo>, SiteError> {
        let url = if url.starts_with("http://") {
            url.replace("http://", "https://")
        } else {
            url.to_string()
        };

        let sub: fautil::Lookup = match self.fapi.lookup_url(&url) {
            Ok(mut results) if !results.is_empty() => results.remove(0),
            _ => {
                return Ok(Some(PostInfo {
                    file_type: get_file_ext(&url).unwrap().to_string(),
                    url: url.clone(),
                    thumb: url.clone(),
                    caption: url,
                    full_url: None,
                    message: None,
                }));
            }
        };

        Ok(Some(PostInfo {
            file_type: get_file_ext(&sub.filename).unwrap().to_string(),
            url: sub.url.clone(),
            thumb: sub.url.clone(),
            caption: format!("https://www.furaffinity.net/view/{}/", sub.id),
            full_url: None,
            message: None,
        }))
    }

    fn load_submission(&self, url: &str) -> Result<Option<PostInfo>, SiteError> {
        let cookies = vec![
            format!("a={}", self.cookies.0),
            format!("b={}", self.cookies.1),
        ]
        .join("; ");

        let resp = self
            .client
            .get(url)
            .header(reqwest::header::COOKIE, cookies)
            .send()?
            .text()?;

        let body = scraper::Html::parse_document(&resp);
        let img = match body.select(&self.submission).next() {
            Some(img) => img,
            None => return Ok(None),
        };

        let image_url = format!("https:{}", img.value().attr("src")?);

        Ok(Some(PostInfo {
            file_type: get_file_ext(&image_url).unwrap().to_string(),
            url: image_url.clone(),
            thumb: image_url.clone(),
            caption: url.to_string(),
            full_url: None,
            message: None,
        }))
    }
}

impl Site for FurAffinity {
    fn name(&self) -> &'static str {
        "FurAffinity"
    }

    fn is_supported(&mut self, url: &str) -> bool {
        url.contains("furaffinity.net/view/")
            || url.contains("furaffinity.net/full/")
            || url.contains("facdn.net/art/")
    }

    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let image = if url.contains("facdn.net/art/") {
            self.load_direct_url(url)
        } else {
            self.load_submission(url)
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

impl Site for Mastodon {
    fn name(&self) -> &'static str {
        "Mastodon"
    }

    fn is_supported(&mut self, url: &str) -> bool {
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
            .send()
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

    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let captures = self.matcher.captures(url).unwrap();

        let base = captures["host"].to_owned();
        let status_id = captures["id"].to_owned();

        let resp = reqwest::Client::new()
            .get(&format!("{}/api/v1/statuses/{}", base, status_id))
            .send();

        let mut resp = match resp {
            Ok(resp) => resp,
            Err(_) => return Err(SiteError {}),
        };

        let json: MastodonStatus = match resp.json() {
            Ok(json) => json,
            Err(_) => return Err(SiteError {}),
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
                    thumb: media.preview_url.clone(),
                    caption: json.url.clone(),
                    full_url: None,
                    message: None,
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

impl Site for Weasyl {
    fn name(&self) -> &'static str {
        "Weasyl"
    }

    fn is_supported(&mut self, url: &str) -> bool {
        self.matcher.is_match(url)
    }

    fn get_images(&mut self, url: &str) -> Result<Option<Vec<PostInfo>>, SiteError> {
        let captures = self.matcher.captures(url).unwrap();
        let sub_id = captures["id"].to_owned();

        let resp: serde_json::Value = reqwest::Client::new()
            .get(&format!(
                "https://www.weasyl.com/api/submissions/{}/view",
                sub_id
            ))
            .header("X-Weasyl-API-Key", self.api_key.as_bytes())
            .send()?
            .json()?;

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
                        thumb: thumb_url,
                        caption: url.to_string(),
                        full_url: None,
                        message: None,
                    }
                })
                .collect(),
        ))
    }
}
