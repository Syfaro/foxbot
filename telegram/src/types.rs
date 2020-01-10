use serde::{Deserialize, Serialize};

use crate::error::*;

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

impl<T> Into<Result<T, Error>> for Response<T> {
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
    pub chosen_inline_result: Option<ChosenInlineResult>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: i32,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatType {
    Private,
    Group,
    Supergroup,
    Channel,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: ChatType,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: MessageEntityType,
    pub offset: i32,
    pub length: i32,
    pub url: Option<String>,
    pub user: Option<User>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageEntityType {
    Mention,
    Hashtag,
    Cashtag,
    BotCommand,
    #[serde(rename = "url")]
    URL,
    Email,
    PhoneNumber,
    Bold,
    Italic,
    Code,
    Pre,
    TextLink,
    TextMention,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Message {
    pub message_id: i32,
    pub from: Option<User>,
    pub date: i64,
    pub chat: Chat,
    pub forward_from: Option<User>,
    pub forward_from_chat: Option<Chat>,
    pub forward_from_message_id: Option<i32>,
    pub forward_signature: Option<String>,
    pub forward_sender_name: Option<String>,
    pub forward_date: Option<i64>,
    pub reply_to_message: Option<Box<Message>>,
    pub edit_date: Option<i64>,
    pub media_group_id: Option<String>,
    pub author_signature: Option<String>,
    pub text: Option<String>,
    pub entities: Option<Vec<MessageEntity>>,
    pub photo: Option<Vec<PhotoSize>>,
    pub caption: Option<String>,
    pub new_chat_members: Option<Vec<User>>,
    pub left_chat_member: Option<User>,
    pub new_chat_title: Option<String>,
    pub migrate_to_chat_id: Option<i64>,
    pub migrate_from_chat_id: Option<i64>,
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
pub struct InlineQuery {
    pub id: String,
    pub from: User,
    pub query: String,
    pub offset: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChosenInlineResult {
    pub result_id: String,
    pub from: User,
    pub inline_message_id: Option<String>,
    pub query: String,
}

#[derive(Deserialize, Debug)]
pub struct File {
    /// The ID for this file, specific to this bot.
    pub file_id: String,
    /// The size of the file, if known.
    pub file_size: Option<usize>,
    /// A path which is required to download the file. It is unclear
    /// when this would ever be `None`.
    pub file_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_inline_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_inline_query_current_chat: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}
