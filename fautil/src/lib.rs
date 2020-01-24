use serde::de::DeserializeOwned;
use std::collections::HashMap;

pub use types::*;

mod types;

/// FAUtil is a collection of methods to get information from fa.huefox.com.
pub struct FAUtil {
    api_key: String,
    client: reqwest::Client,
}

#[derive(PartialEq)]
pub enum MatchType {
    Close,
    Exact,
    Force,
}

impl FAUtil {
    pub const API_ENDPOINT: &'static str = "https://api.fuzzysearch.net";

    /// Create a new FAUtil instance. Requires the API key.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Makes a request against the API. It deserializes the JSON response.
    /// Generally not used as there are more specific methods available.
    async fn make_request<T: Default + DeserializeOwned>(
        &self,
        endpoint: &str,
        params: &HashMap<&str, String>,
    ) -> reqwest::Result<T> {
        let url = format!("{}{}", Self::API_ENDPOINT, endpoint);

        self.client
            .get(&url)
            .header("X-Api-Key", self.api_key.as_bytes())
            .query(params)
            .send()
            .await?
            .json()
            .await
    }

    /// Attempt to look up an image by its URL. Note that URLs should be https.
    pub async fn lookup_url(&self, url: &str) -> reqwest::Result<Vec<File>> {
        let mut params = HashMap::new();
        params.insert("url", url.to_string());

        self.make_request("/file", &params).await
    }

    /// Attempt to look up an image by its original name on FA.
    pub async fn lookup_filename(&self, filename: &str) -> reqwest::Result<Vec<File>> {
        let mut params = HashMap::new();
        params.insert("name", filename.to_string());

        self.make_request("/file", &params).await
    }

    /// Attempt to lookup multiple hashes.
    pub async fn lookup_hashes(&self, hashes: Vec<i64>) -> reqwest::Result<Vec<File>> {
        let mut params = HashMap::new();
        params.insert(
            "hashes",
            hashes
                .iter()
                .map(|hash| hash.to_string())
                .collect::<Vec<_>>()
                .join(","),
        );

        self.make_request("/hashes", &params).await
    }

    /// Attempt to reverse image search.
    ///
    /// Requiring an exact match will be faster, but potentially leave out results.
    pub async fn image_search(&self, data: &[u8], exact: MatchType) -> reqwest::Result<Matches> {
        use reqwest::multipart::{Form, Part};

        let url = format!("{}/image", Self::API_ENDPOINT);

        let part = Part::bytes(Vec::from(data));
        let form = Form::new().part("image", part);

        let query = match exact {
            MatchType::Exact => vec![("type", "exact".to_string())],
            MatchType::Force => vec![("type", "force".to_string())],
            _ => vec![("type", "close".to_string())],
        };

        self.client
            .post(&url)
            .query(&query)
            .header("X-Api-Key", self.api_key.as_bytes())
            .multipart(form)
            .send()
            .await?
            .json()
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_api() -> FAUtil {
        FAUtil::new("eluIOaOhIP1RXlgYetkcZCF8la7p3NoCPy8U0i8dKiT4xdIH".to_string())
    }

    #[test]
    fn test_lookup() {
        let api = get_api();

        let no_filenames = api.lookup_filename("nope");
        println!("{:?}", no_filenames);

        assert!(no_filenames.is_ok());
        assert_eq!(no_filenames.unwrap().len(), 0);
    }

    #[test]
    fn test_image() {
        let api = get_api();

        let images = api.image_search(
            vec![
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
                0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
                0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
            ],
            MatchType::Close,
        );
        println!("{:?}", images);

        assert!(images.is_ok());
        assert_eq!(images.unwrap().len(), 10);
    }
}
