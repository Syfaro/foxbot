use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i32,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
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
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
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
pub struct User {
    pub id: i32,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub migrate_to_chat_id: Option<i64>,
    pub retry_after: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct Response<T> {
    pub ok: bool,
    pub error_code: Option<i32>,
    pub description: Option<String>,
    pub parameters: Option<ResponseParameters>,

    pub result: Option<T>,
}

pub struct Telegram {
    api_key: String,
    client: reqwest::Client,
}

pub trait TelegramRequest: serde::Serialize {
    type Response: serde::de::DeserializeOwned + std::fmt::Debug;

    fn endpoint(&self) -> &str;

    fn values(&self) -> serde_json::Value {
        serde_json::to_value(&self).unwrap()
    }
}

#[derive(Debug)]
pub struct Error {
    error_code: Option<i32>,
    description: Option<String>,
    parameters: Option<ResponseParameters>,
}

#[derive(Serialize)]
pub struct GetMe;

impl TelegramRequest for GetMe {
    type Response = User;

    fn endpoint(&self) -> &str {
        "getMe"
    }
}

#[derive(Serialize, Default)]
pub struct GetUpdates {
    pub offset: Option<i32>,
    pub limit: Option<i32>,
    pub timeout: Option<i32>,
    pub allowed_updates: Option<Vec<String>>,
}

impl TelegramRequest for GetUpdates {
    type Response = Vec<Update>;

    fn endpoint(&self) -> &str {
        "getUpdates"
    }
}

#[derive(Serialize, Default)]
pub struct SendMessage {
    pub chat_id: i64,
    pub text: String,
}

impl TelegramRequest for SendMessage {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendMessage"
    }
}

impl Telegram {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder().timeout(None).build().unwrap();

        let bot_api = Self { api_key, client };

        bot_api.make_request(&GetMe {}).unwrap();

        bot_api
    }

    pub fn make_request<T>(&self, request: &T) -> Result<T::Response, Error>
    where
        T: TelegramRequest,
    {
        let endpoint = request.endpoint();

        let url = format!("https://api.telegram.org/bot{}/{}", self.api_key, endpoint);

        let values = request.values();

        log::debug!("Making request to {} with data {:?}", endpoint, values);

        let resp: Response<T::Response> = self
            .client
            .post(&url)
            .json(&values)
            .send()
            .unwrap()
            .json()
            .unwrap();

        log::debug!("Got response from {} with data {:?}", endpoint, resp);

        if resp.ok {
            Ok(resp.result.unwrap())
        } else {
            Err(Error {
                error_code: resp.error_code,
                description: resp.description,
                parameters: resp.parameters,
            })
        }
    }
}
