#[derive(Debug)]
pub struct PostInfo {
    file_type: String,
    url: String,
    thumb: String,
    caption: String,

    full_url: Option<String>,
    message: Option<String>,
}

fn get_file_ext(name: &str) -> Option<&str> {
    name.split('.').last()
}

#[derive(Debug, Clone)]
pub struct SiteError;

pub trait Site {
    fn name(&self) -> &'static str;
    fn is_supported(&self, url: &str) -> bool;
    fn get_images(&self, url: &str) -> Result<Vec<PostInfo>, SiteError>;
}

pub struct Direct;

impl Direct {
    const EXTENSIONS: &'static [&'static str] = &["png", "jpg", "jpeg", "gif"];
    const TYPES: &'static [&'static str] = &["image/png", "image/jpeg", "image/gif"];
}

impl Site for Direct {
    fn name(&self) -> &'static str {
        "direct links"
    }

    fn is_supported(&self, url: &str) -> bool {
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

    fn get_images(&self, url: &str) -> Result<Vec<PostInfo>, SiteError> {
        let u = url.to_string();

        Ok(vec![PostInfo {
            file_type: get_file_ext(url).unwrap().to_string(),
            url: u.clone(),
            thumb: u.clone(),
            caption: u,
            full_url: None,
            message: None,
        }])
    }
}

pub struct FurAffinity {
    cookies: Option<(String, String)>,
}

impl FurAffinity {
    pub fn new(cookies: Option<(String, String)>) -> Self {
        Self { cookies }
    }

    fn load_direct_url() -> Option<PostInfo> {
        unimplemented!();
    }

    fn load_submission() -> Option<PostInfo> {
        unimplemented!();
    }
}

impl Site for FurAffinity {
    fn name(&self) -> &'static str {
        "FurAffinity"
    }

    fn is_supported(&self, url: &str) -> bool {
        url.contains("furaffinity.net/view/")
            || url.contains("furaffinity.net/full/")
            || url.contains("facdn.net/art/")
    }

    fn get_images(&self, url: &str) -> Result<Vec<PostInfo>, SiteError> {
        unimplemented!();
    }
}
