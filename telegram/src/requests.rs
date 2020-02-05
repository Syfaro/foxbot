use serde::{Deserialize, Serialize};

use crate::files::*;
use crate::types::*;
use crate::TelegramRequest;

/// ChatID represents a possible type of value for requests.
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum ChatID {
    /// A chat's numeric ID.
    Identifier(i64),
    /// A username for a channel.
    Username(String),
}

impl Message {
    pub fn chat_id(&self) -> ChatID {
        ChatID::Identifier(self.chat.id)
    }
}

impl From<i64> for ChatID {
    fn from(item: i64) -> Self {
        ChatID::Identifier(item)
    }
}

impl From<i32> for ChatID {
    fn from(item: i32) -> Self {
        ChatID::Identifier(item as i64)
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

/// ForceReply allows you to default users to replying to your message.
#[derive(Serialize, Debug)]
pub struct ForceReply {
    /// This must be set to `true` to operate correctly.
    pub force_reply: bool,
    /// If only the user you are mentioning or replying to should be defaulted
    /// to replying to your message, or if it should default to replying for all
    /// members of the chat.
    pub selective: bool,
}

impl ForceReply {
    /// Create a [ForceReply] with selectivity.
    pub fn selective() -> Self {
        Self {
            force_reply: true,
            selective: true,
        }
    }
}

impl Default for ForceReply {
    /// Create a [ForceReply] without selectivity.
    fn default() -> Self {
        Self {
            force_reply: true,
            selective: false,
        }
    }
}

/// ReplyMarkup is additional data sent with a [Message] to enhance the bot
/// user experience.
///
/// You may add one of the following:
/// * [InlineKeyboardMarkup]
/// * <s>ReplyKeyboardMarkup</s> // TODO
/// * <s>ReplyKeyboardRemove</s> // TODO
/// * [ForceReply]
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum ReplyMarkup {
    InlineKeyboardMarkup(InlineKeyboardMarkup),
    ForceReply(ForceReply),
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub enum ParseMode {
    HTML,
    Markdown,
    MarkdownV2,
}

/// ChatAction is the action that the bot is indicating.
#[derive(Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ChatAction {
    Typing,
    UploadPhoto,
}

#[derive(Debug, Serialize, Clone)]
pub struct InputMediaPhoto {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Default for InputMediaPhoto {
    fn default() -> Self {
        Self {
            media_type: "photo".to_string(),
            media: Default::default(),
            caption: None,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct InputMediaVideo {
    #[serde(rename = "type")]
    pub media_type: String,
    pub media: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Default for InputMediaVideo {
    fn default() -> Self {
        Self {
            media_type: "video".to_string(),
            media: Default::default(),
            caption: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum InputMedia {
    Photo(InputMediaPhoto),
    Video(InputMediaVideo),
}

impl InputMedia {
    /// Replaces the media within an InputMedia without caring about the type.
    pub fn update_media(&self, media: FileType) -> Self {
        match self {
            InputMedia::Photo(photo) => InputMedia::Photo(InputMediaPhoto {
                media,
                ..photo.clone()
            }),
            InputMedia::Video(video) => InputMedia::Video(InputMediaVideo {
                media,
                ..video.clone()
            }),
        }
    }
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

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<InlineKeyboardMarkup>,
    #[serde(flatten)]
    pub content: InlineQueryType,
}

#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum InlineQueryType {
    Article(InlineQueryResultArticle),
    Photo(InlineQueryResultPhoto),
    GIF(InlineQueryResultGIF),
    Video(InlineQueryResultVideo),
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultArticle {
    pub title: String,
    #[serde(flatten)]
    pub input_message_content: InputMessageType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultPhoto {
    pub photo_url: String,
    pub thumb_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultGIF {
    pub gif_url: String,
    pub thumb_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InlineQueryResultVideo {
    pub video_url: String,
    pub mime_type: String,
    pub thumb_url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl InlineQueryResult {
    pub fn article(id: String, title: String, text: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "article".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Article(InlineQueryResultArticle {
                title,
                description: None,
                input_message_content: InputMessageType::Text(InputMessageText {
                    message_text: text,
                    parse_mode: None,
                }),
            }),
        }
    }

    pub fn photo(id: String, photo_url: String, thumb_url: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "photo".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Photo(InlineQueryResultPhoto {
                photo_url,
                thumb_url,
                caption: None,
            }),
        }
    }

    pub fn gif(id: String, gif_url: String, thumb_url: String) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "gif".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::GIF(InlineQueryResultGIF {
                gif_url,
                thumb_url,
                caption: None,
            }),
        }
    }

    pub fn video(
        id: String,
        video_url: String,
        mime_type: String,
        thumb_url: String,
        title: String,
    ) -> InlineQueryResult {
        InlineQueryResult {
            result_type: "video".into(),
            id,
            reply_markup: None,
            content: InlineQueryType::Video(InlineQueryResultVideo {
                video_url,
                mime_type,
                thumb_url,
                title,
                caption: None,
            }),
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum InputMessageType {
    Text(InputMessageText),
}

#[derive(Serialize, Debug, Clone)]
pub struct InputMessageText {
    pub message_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

/// GetMe is a request that returns [User] information for the current bot.
#[derive(Serialize, Debug)]
pub struct GetMe;

impl TelegramRequest for GetMe {
    type Response = User;

    fn endpoint(&self) -> &str {
        "getMe"
    }
}

/// GetUpdates is a request that returns any available [Updates](Update).
#[derive(Serialize, Default, Debug)]
pub struct GetUpdates {
    /// ID for the first update to return. This must be set to one higher
    /// than previous IDs in order to confirm previous updates and clear them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
    /// Maximum number of [Updates](Update) to retrieve. May be set 1-100,
    /// defaults to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    /// Number of seconds for long polling. This should be set to a reasonable
    /// value in production to avoid unneeded requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i32>,
    /// Which update types to receive. May be set to any available types.
    /// * `message`
    /// * `edited_message`
    /// * `channel_post`
    /// * `edited_channel_post`
    /// * `inline_query`
    /// * `chosen_inline_result`
    /// * `callback_query`
    /// * `shipping_query`
    /// * `pre_checkout_query`
    /// * `poll`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_updates: Option<Vec<String>>,
}

impl TelegramRequest for GetUpdates {
    type Response = Vec<Update>;

    fn endpoint(&self) -> &str {
        "getUpdates"
    }
}

/// SendMessage sends a message.
#[derive(Serialize, Default, Debug)]
pub struct SendMessage {
    /// The ID of the chat to send a message to.
    pub chat_id: ChatID,
    /// The text of the message.
    pub text: String,
    /// The mode used to parse the provided text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<ParseMode>,
    /// The ID of the [Message] this Message is in reply to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
    /// The [ReplyMarkup], if desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
    /// If Telegram should not generate a web page preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_web_page_preview: Option<bool>,
}

impl TelegramRequest for SendMessage {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendMessage"
    }
}

/// SendChatAction allows you to indicate to users that the bot is performing
/// an action.
///
/// Actions last for 5 seconds or until a message is sent,
/// whichever comes first.
#[derive(Serialize, Debug)]
pub struct SendChatAction {
    /// The ID of the chat to send an action to.
    pub chat_id: ChatID,
    /// The action to indicate.
    pub action: ChatAction,
}

impl TelegramRequest for SendChatAction {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "sendChatAction"
    }
}

/// SendPhoto sends a photo.
#[derive(Serialize, Debug, Default)]
pub struct SendPhoto {
    /// The ID of the chat to send a photo to.
    pub chat_id: ChatID,
    /// The file that makes up this photo.
    #[serde(skip_serializing_if = "FileType::needs_upload")]
    pub photo: FileType,
    /// A caption for the photo, if desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl TelegramRequest for SendPhoto {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendPhoto"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        // Check if the photo needs to be uploaded. If the photo does need to
        // be uploaded, we specify the field name and get the file. This unwrap
        // is safe because `needs_upload` only returns true when it exists.
        if self.photo.needs_upload() {
            Some(vec![("photo".to_owned(), self.photo.file().unwrap())])
        } else {
            None
        }
    }
}

#[derive(Serialize, Debug, Default)]
pub struct SendVideo {
    pub chat_id: ChatID,
    #[serde(skip_serializing_if = "FileType::needs_upload")]
    pub video: FileType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
}

impl TelegramRequest for SendVideo {
    type Response = Message;

    fn endpoint(&self) -> &str {
        "sendVideo"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        // Check if the photo needs to be uploaded. If the photo does need to
        // be uploaded, we specify the field name and get the file. This unwrap
        // is safe because `needs_upload` only returns true when it exists.
        if self.video.needs_upload() {
            Some(vec![("photo".to_owned(), self.video.file().unwrap())])
        } else {
            None
        }
    }
}

/// GetFile retrieves information about a file.
///
/// This will not download the file! It only returns a [File] containing
/// the path which is needed to download the file. This returned ID lasts at
/// least one hour.
#[derive(Serialize, Debug)]
pub struct GetFile {
    /// The ID of the file to fetch.
    pub file_id: String,
}

impl TelegramRequest for GetFile {
    type Response = File;

    fn endpoint(&self) -> &str {
        "getFile"
    }
}

#[derive(Debug, Serialize, Default)]
pub struct SendMediaGroup {
    pub chat_id: ChatID,
    #[serde(serialize_with = "clean_input_media")]
    pub media: Vec<InputMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i32>,
}

impl TelegramRequest for SendMediaGroup {
    type Response = Vec<Message>;

    fn endpoint(&self) -> &str {
        "sendMediaGroup"
    }

    fn files(&self) -> Option<Vec<(String, reqwest::multipart::Part)>> {
        if !self.media.iter().any(|item| item.get_file().needs_upload()) {
            return None;
        }

        let mut items = Vec::new();

        for item in &self.media {
            let file = item.get_file();

            let part = match file {
                FileType::Bytes(file_name, bytes) => {
                    let file =
                        reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone());

                    (file_name.to_owned(), file)
                }
                _ => continue,
            };

            items.push(part);
        }

        Some(items)
    }
}

#[derive(Debug, Serialize, Default)]
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

#[derive(Clone, Debug, Serialize)]
pub struct SetWebhook {
    pub url: String,
}

impl TelegramRequest for SetWebhook {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "setWebhook"
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeleteWebhook;

impl TelegramRequest for DeleteWebhook {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "deleteWebhook"
    }
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct AnswerCallbackQuery {
    pub callback_query_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_alert: Option<bool>,
}

impl TelegramRequest for AnswerCallbackQuery {
    type Response = bool;

    fn endpoint(&self) -> &str {
        "answerCallbackQuery"
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MessageOrBool {
    Message(Message),
    Bool(bool),
}

#[derive(Default, Debug, Serialize)]
pub struct EditMessageText {
    pub chat_id: ChatID,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_message_id: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<ParseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_web_page_preview: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl TelegramRequest for EditMessageText {
    type Response = MessageOrBool;

    fn endpoint(&self) -> &str {
        "editMessageText"
    }
}

#[derive(Default, Debug, Serialize)]
pub struct EditMessageCaption {
    pub chat_id: ChatID,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl TelegramRequest for EditMessageCaption {
    type Response = MessageOrBool;

    fn endpoint(&self) -> &str {
        "editMessageCaption"
    }
}

#[derive(Default, Debug, Serialize)]
pub struct EditMessageReplyMarkup {
    pub chat_id: ChatID,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<ReplyMarkup>,
}

impl TelegramRequest for EditMessageReplyMarkup {
    type Response = MessageOrBool;

    fn endpoint(&self) -> &str {
        "editMessageReplyMarkup"
    }
}
