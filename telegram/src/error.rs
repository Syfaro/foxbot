use serde::Deserialize;

#[derive(Fail, Debug)]
pub enum Error {
    #[fail(display = "telegram error: {}", _0)]
    Telegram(TelegramError),
    #[fail(display = "json parsing error: {}", _0)]
    JSON(serde_json::Error),
    #[fail(display = "http error: {}", _0)]
    Request(reqwest::Error),
}

impl From<reqwest::Error> for Error {
    fn from(item: reqwest::Error) -> Error {
        Error::Request(item)
    }
}

impl From<serde_json::Error> for Error {
    fn from(item: serde_json::Error) -> Error {
        Error::JSON(item)
    }
}

impl From<TelegramError> for Error {
    fn from(item: TelegramError) -> Error {
        Error::Telegram(item)
    }
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub migrate_to_chat_id: Option<i64>,
    pub retry_after: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramError {
    /// A HTTP-style error code.
    pub error_code: Option<i32>,
    /// A human readable error description.
    pub description: Option<String>,
    /// Additional information about errors in the request.
    pub parameters: Option<ResponseParameters>,
}

impl std::fmt::Display for TelegramError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Telegram Error {}: {}",
            self.error_code.unwrap_or(-1),
            self.description
                .clone()
                .unwrap_or_else(|| "no description".to_string())
        )
    }
}
