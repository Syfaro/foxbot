use serde::Deserialize;

/// FAUtil is a collection of methods to get information from fa.huefox.com.
pub struct FAUtil {
    api_key: String,
    client: reqwest::Client,
}

/// Lookup is information returned when attempting to search for a file by
/// url or filename. It contains no additional information about the image
/// besides its ID on FurAffinity.
#[derive(Debug, Deserialize)]
pub struct Lookup {
    pub id: usize,
    pub url: String,
    pub filename: String,
}

/// ImageLookup is information returned when attempting to reverse image search.
/// It includes a distance, which is the hamming distance between the provided
/// image and the image in the database.
#[derive(Debug, Deserialize)]
pub struct ImageLookup {
    pub id: usize,
    pub distance: usize,
    pub url: String,
    pub filename: String,
}

impl FAUtil {
    pub const API_ENDPOINT: &'static str = "https://fa.huefox.com/api/v1/";

    /// Create a new FAUtil instance. Requires the API key.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Makes a request against the API. It deserializes the JSON response.
    /// Generally not used as there are more specific methods available.
    async fn make_request<T: Default + serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        params: &std::collections::HashMap<&str, &str>,
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
    pub async fn lookup_url(&self, url: &str) -> reqwest::Result<Vec<Lookup>> {
        let mut params = std::collections::HashMap::new();
        params.insert("url", url);

        self.make_request("url", &params).await
    }

    /// Attempt to look up an image by its original name on FA.
    pub async fn lookup_filename(&self, filename: &str) -> reqwest::Result<Vec<Lookup>> {
        let mut params = std::collections::HashMap::new();
        params.insert("file", filename);

        self.make_request("file", &params).await
    }

    /// Attempt to reverse image search.
    ///
    /// Requiring an exact match will be faster, but potentially leave out results.
    pub async fn image_search(
        &self,
        data: Vec<u8>,
        exact: bool,
    ) -> reqwest::Result<Vec<ImageLookup>> {
        let url = format!("{}image", Self::API_ENDPOINT);

        let part = reqwest::multipart::Part::bytes(data);
        let form = reqwest::multipart::Form::new().part("image", part);

        let query = if exact {
            vec![("exact", "true")]
        } else {
            vec![]
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

        let many_filenames = api.lookup_filename("%.png");
        println!("{:?}", many_filenames);

        assert!(many_filenames.is_ok());
        assert_eq!(many_filenames.unwrap().len(), 10);
    }

    #[test]
    fn test_image() {
        let api = get_api();

        let images = api.image_search(vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]);
        println!("{:?}", images);

        assert!(images.is_ok());
        assert_eq!(images.unwrap().len(), 10);
    }
}
