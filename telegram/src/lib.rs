#![feature(bind_by_move_pattern_guards)]

use serde_derive::{Deserialize, Serialize};

#[derive(Debug)]
pub enum Error {
    Telegram(TelegramError),
    JSON(serde_json::Error),
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
pub struct TelegramError {
    /// A HTTP-style error code.
    pub error_code: Option<i32>,
    /// A human readable error description.
    pub description: Option<String>,
    /// Additional information about errors in the request.
    pub parameters: Option<ResponseParameters>,
}

#[derive(Debug, Deserialize)]
pub struct Response<T> {
    /// If the request was successful. If true, the result is available.
    /// If false, error contains information about what happened.
    pub ok: bool,
    #[serde(flatten)]
    pub error: TelegramError,

    /// The response data.
    pub result: Option<T>,
}

/// Allow for turning a Response into a more usable Result type.
impl<T> Into<Result<T, TelegramError>> for Response<T> {
    fn into(self) -> Result<T, TelegramError> {
        match self.result {
            Some(result) if self.ok => Ok(result),
            _ => Err(self.error),
        }
    }
}

impl <T> Into<Result<T, Error>> for Response<T> {
    fn into(self) -> Result<T, Error> {
        match self.result {
            Some(result) if self.ok => Ok(result),
            _ => Err(Error::Telegram(self.error)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i32,
    pub message: Option<Message>,
    pub inline_query: Option<InlineQuery>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i32,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i32,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
}

#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub migrate_to_chat_id: Option<i64>,
    pub retry_after: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct InlineQuery {
    pub id: String,
    pub from: User,
    pub query: String,
    pub offset: String,
}

/// A trait for all Telegram requests. It has as many default methods as
/// possible but still requires some additions.
pub trait TelegramRequest: serde::Serialize {
    /// Response is the type used when Deserializing Telegram's result field.
    ///
    /// For convenience of debugging, it must implement Debug.
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

/// ChatID represents a possible type of value for requests.
#[derive(Serialize)]
#[serde(untagged)]
pub enum ChatID {
    /// A chat's numeric ID.
    Identifier(i64),
    /// A username for a channel.
    Username(String),
}

impl From<i64> for ChatID {
    fn from(item: i64) -> Self {
        ChatID::Identifier(item)
    }
}

impl From<String> for ChatID {
    fn from(item: String) -> Self {
        ChatID::Username(item)
    }
}

impl From<&str> for ChatID {
    fn from(item: &str) -> Self {
        ChatID::Username(item.to_string())
    }
}

impl Default for ChatID {
    fn default() -> Self {
        ChatID::Identifier(0)
    }
}

/// GetMe is a request that returns a User about the current bot.
#[derive(Serialize)]
pub struct GetMe;

impl TelegramRequest for GetMe {
    type Response = User;

    fn endpoint(&self) -> &str {
        "getMe"
    }
}

/// GetUpdates is a request that returns any available Updates.
#[derive(Serialize, Default)]
pub struct GetUpdates {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_updates: Option<Vec<String>>,
}

impl TelegramRequest for GetUpdates {
    type Response = Vec<Update>;

    fn endpoint(&self) -> &str {
        "getUpdates"
    }
}

/// SendMessage, well, sends a message.
#[derive(Serialize, Default)]
pub struct SendMessage {
    pub chat_id: ChatID,
    pub text: String,
}

impl TelegramRequest for SendMessage {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendMessage"
    }
}

/// FileType is a possible file for a Telegram request.
#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum FileType {
    /// URL contains a String with the URL to pass to Telegram.
    URL(String),
    /// FileID is an ID about a file already on Telegram's servers.
    FileID(String),
    /// Attach is a specific type used to attach multiple files to a request.
    Attach(String),
    /// Bytes requires a filename in addition to the file bytes, which it
    /// then uploads to Telegram.
    Bytes(String, Vec<u8>),
}

impl std::fmt::Debug for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FileType::URL(url) => write!(f, "FileType URL: {}", url),
            FileType::FileID(file_id) => write!(f, "FileType FileID: {}", file_id),
            FileType::Attach(attach) => write!(f, "FileType Attach: {}", attach),
            FileType::Bytes(name, bytes) => write!(f, "FileType Bytes: {} with len {}", name, bytes.len()),
        }
    }
}

impl FileType {
    /// Returns if this file is a type that gets uploaded to Telegram.
    /// Most types are simply passed through as strings.
    pub fn needs_upload(&self) -> bool {
        match self {
            FileType::Bytes(_, _) => true,
            _ => false,
        }
    }

    /// Get a multipart Part for the file.
    /// Note that this panics if called for a type that does not need uploading.
    pub fn file(&self) -> reqwest::multipart::Part {
        match self {
            FileType::Bytes(file_name, bytes) =>
                reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name(file_name.clone()),
            _ => unimplemented!(),
        }
    }
}

#[derive(Serialize)]
pub struct SendPhoto {
    pub chat_id: ChatID,
    #[serde(skip_serializing_if = "FileType::needs_upload")]
    pub photo: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl TelegramRequest for SendPhoto {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendPhoto"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        if self.photo.needs_upload() {
            Some(vec![("photo".to_owned(), self.photo.file())])
        } else {
            None
        }
    }
}

#[derive(Serialize, Clone)]
pub struct InputMediaPhoto {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct InputMediaVideo {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum InputMedia {
    Photo(InputMediaPhoto),
    Video(InputMediaVideo),
}

impl InputMedia {
    /// Get the file out of an InputMedia value.
    pub fn get_file(&self) -> &FileType {
        match self {
            InputMedia::Photo(photo) => &photo.media,
            InputMedia::Video(video) => &video.media,
        }
    }
}

#[derive(Serialize)]
pub struct SendMediaGroup {
    pub chat_id: ChatID,
    #[serde(serialize_with = "clean_input_media")]
    pub media: Vec<InputMedia>,
}

/// Attempt to remove body for types that are getting uploaded.
/// It also converts files into attachments with names based on filenames.
fn clean_input_media<S>(input_media: &[InputMedia], s: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
    use serde::ser::SerializeSeq;

    let mut seq = s.serialize_seq(Some(input_media.len()))?;

    for elem in input_media {
        let file = elem.get_file();
        if file.needs_upload() {
            let new_file = match file {
                FileType::Bytes(file_name, _) => FileType::Attach(format!("attach://{}", file_name)),
                _ => unimplemented!(),
            };

            let new_elem = match elem {
                InputMedia::Photo(photo) => InputMedia::Photo(InputMediaPhoto{
                    media: new_file,
                    ..photo.clone()
                }),
                InputMedia::Video(video) => InputMedia::Video(InputMediaVideo{
                    media: new_file,
                    ..video.clone()
                }),
            };

            seq.serialize_element(&new_elem)?;
        } else {
            seq.serialize_element(elem)?;
        }
    }

    seq.end()
}

impl TelegramRequest for SendMediaGroup {
    type Response = Vec<Message>;

    fn endpoint(&self) -> &str {
        "sendMediaGroup"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        if !self.media.iter().any(|item| {
            item.get_file().needs_upload()
        }) {
            return None
        }

        let mut items = Vec::new();

        for item in &self.media {
            let file = item.get_file();

            let part = match file {
                FileType::Bytes(file_name, bytes) => {
                    let file = reqwest::multipart::Part::bytes(bytes.clone())
                        .file_name(file_name.clone());

                    (file_name.to_owned(), file)
                },
                _ => continue,
            };

            items.push(part);
        }

        Some(items)
    }
}

#[derive(Serialize)]
pub struct AnswerInlineQuery {
    pub inline_query_id: String,
    pub results: Vec<InlineQueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_time: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_personal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_pm_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_pm_parameter: Option<String>,
}

impl TelegramRequest for AnswerInlineQuery {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "answerInlineQuery"
    }
}

#[derive(Serialize, Debug)]
pub struct InlineKeyboardButton {
    pub text: String,
    pub url: Option<String>,
    pub callback_data: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Serialize, Debug)]
pub struct InlineQueryResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub id: String,
    pub reply_markup: Option<InlineKeyboardMarkup>,
    #[serde(flatten)]
    pub content: InlineQueryType,
}

#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum InlineQueryType {
    Photo(InlineQueryResultPhoto),
}

#[derive(Serialize, Debug)]
pub struct InlineQueryResultPhoto {
    pub photo_url: String,
    pub thumb_url: String,
}

impl InlineQueryResult {
    pub fn photo(id: String, photo_url: String, thumb_url: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "photo".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Photo(InlineQueryResultPhoto{
                photo_url,
                thumb_url,
            }),
        }
    }
}

pub struct Telegram {
    api_key: String,
    client: reqwest::Client,
}

impl Telegram {
    /// Create a new Telegram instance with a specified API key.
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder().timeout(None).build().unwrap();

        Self { api_key, client }
    }

    /// Makes a request for a TelegramRequest item and parses the response
    /// into the requested output type if the request succeeded.
    pub fn make_request<T>(&self, request: &T) -> Result<T::Response, Error>
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

            let form = form_values.iter().fold(reqwest::multipart::Form::new(), |form, (name, value)| {
                if let Ok(value) = serde_json::to_string(value) {
                    form.text(name.to_owned(), value)
                } else {
                    log::warn!("Skipping field {} due to invalid data: {:?}", name, value);
                    form
                }
            });

            let form = files.into_iter().fold(form, |form, (name, part)| {
                form.part(name, part)
            });

            let mut resp = self
                .client
                .post(&url)
                .multipart(form)
                .send()?;

            log::trace!("Got response from {} with data {:?}", endpoint, resp);

            resp
                .json()?
        } else {
            // No files to upload, use a JSON body in a POST request to the
            // requested endpoint.

            self
                .client
                .post(&url)
                .json(&values)
                .send()?
                .json()?
        };

        log::debug!("Got response from {} with data {:?}", endpoint, resp);

        resp.into()
    }
}
