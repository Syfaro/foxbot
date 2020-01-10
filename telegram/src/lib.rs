pub use error::*;
pub use files::*;
pub use requests::*;
pub use types::*;

#[macro_use]
extern crate failure;

mod error;
mod files;
mod requests;
mod types;

/// A trait for all Telegram requests. It has as many default methods as
/// possible but still requires some additions.
pub trait TelegramRequest: serde::Serialize {
    /// Response is the type used when Deserializing Telegram's result field.
    ///
    /// For convenience of debugging, it must implement [Debug](std::fmt::Debug).
    type Response: serde::de::DeserializeOwned + std::fmt::Debug;

    /// Endpoint to use for the request.
    fn endpoint(&self) -> &str;

    /// A JSON-compatible serialization of the data to send with the request.
    /// The default works for most methods.
    fn values(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self)
    }

    /// Files that are sent with the request.
    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        None
    }
}

pub struct Telegram {
    api_key: String,
    client: reqwest::Client,
}

impl Telegram {
    /// Create a new Telegram instance with a specified API key.
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder().build().unwrap();

        Self { api_key, client }
    }

    /// Make a request for a [TelegramRequest] item and parse the response
    /// into the requested output type if the request succeeded.
    pub async fn make_request<T>(&self, request: &T) -> Result<T::Response, Error>
    where
        T: TelegramRequest,
    {
        let endpoint = request.endpoint();

        let url = format!("https://api.telegram.org/bot{}/{}", self.api_key, endpoint);
        let values = request.values()?;

        log::debug!("Making request to {} with data {:?}", endpoint, values);

        let resp: Response<T::Response> = if let Some(files) = request.files() {
            // If our request has a file that needs to be uploaded, use
            // a multipart upload. Works by converting each JSON value into
            // a string and putting it into a field with the same name as the
            // original object.

            let mut form_values = serde_json::Map::new();
            form_values = values.as_object().unwrap_or_else(|| &form_values).clone();

            let form =
                form_values
                    .iter()
                    .fold(reqwest::multipart::Form::new(), |form, (name, value)| {
                        if let Ok(value) = serde_json::to_string(value) {
                            form.text(name.to_owned(), value)
                        } else {
                            log::warn!("Skipping field {} due to invalid data: {:?}", name, value);
                            form
                        }
                    });

            let form = files
                .into_iter()
                .fold(form, |form, (name, part)| form.part(name, part));

            let resp = self.client.post(&url).multipart(form).send().await?;

            log::trace!("Got response from {} with data {:?}", endpoint, resp);

            resp.json().await?
        } else {
            // No files to upload, use a JSON body in a POST request to the
            // requested endpoint.

            self.client
                .post(&url)
                .json(&values)
                .send()
                .await?
                .json()
                .await?
        };

        log::debug!("Got response from {} with data {:?}", endpoint, resp);

        resp.into()
    }

    /// Download a file from Telegram's servers.
    ///
    /// It requires a file path which can be obtained with [GetFile].
    pub async fn download_file(&self, file_path: String) -> Result<Vec<u8>, Error> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.api_key, file_path
        );

        Ok(self.client.get(&url).send().await?.bytes().await?.to_vec())
    }
}
