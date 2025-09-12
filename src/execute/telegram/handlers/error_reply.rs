use async_trait::async_trait;

use crate::{execute::telegram::Context, needs_field, Error};

use super::{
    Handler,
    Status::{self, *},
};

pub struct ErrorReplyHandler {
    client: reqwest::Client,
}

impl ErrorReplyHandler {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("Unable to create client");

        Self { client }
    }
}

#[async_trait]
impl Handler for ErrorReplyHandler {
    fn name(&self) -> &'static str {
        "error_reply"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error> {
        let message = needs_field!(update, message);
        let text = needs_field!(message, text);
        let reply_message = needs_field!(message, reply_to_message);
        let reply_message_from = needs_field!(reply_message, from);
        let reply_message_text = needs_field!(reply_message, text);
        let entities = needs_field!(reply_message, entities);

        let (sentry_url, organization, project) = match (
            cx.config.sentry_url.as_deref(),
            cx.config.sentry_organization_slug.as_deref(),
            cx.config.sentry_project_slug.as_deref(),
        ) {
            (Some(sentry_url), Some(organization), Some(project)) => {
                (sentry_url, organization, project)
            }
            _ => return Ok(Completed),
        };

        // Only want to look at messages that are replies to this bot
        if reply_message_from.id != cx.bot_user.id {
            return Ok(Ignored);
        }

        let code = match get_code_block(entities, reply_message_text) {
            Some(code) => code,
            _ => return Ok(Ignored),
        };

        let auth = format!("DSN {}", sentry_url);

        let data = SentryFeedback {
            comments: text.to_string(),
            event_id: code,
            // This field is required, but Telegram doesn't give us emails...
            email: "telegram-user@example.com".to_string(),
            name: message
                .from
                .as_ref()
                .map(|from| from.username.clone().unwrap_or_else(|| from.id.to_string())),
        };

        self.client
            .post(&format!(
                "https://sentry.io/api/0/projects/{}/{}/user-feedback/",
                organization, project
            ))
            .json(&data)
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await?;

        cx.send_generic_reply(message, "error-feedback").await?;

        Ok(Completed)
    }
}

#[derive(serde::Serialize)]
struct SentryFeedback {
    comments: String,
    event_id: String,
    name: Option<String>,
    email: String,
}

fn get_code_block(entities: &[tgbotapi::MessageEntity], text: &str) -> Option<String> {
    // Find any code blocks, ignore if there's more than one
    let code_blocks = entities
        .iter()
        .filter(|entity| entity.entity_type == tgbotapi::MessageEntityType::Code)
        .collect::<Vec<_>>();
    if code_blocks.len() != 1 {
        return None;
    }

    // Make sure the code block is the correct length
    let entity = code_blocks[0];
    if entity.length != 36 {
        return None;
    }

    // Iterate the text of the message this is replying to in order to
    // get the event ID
    let code = text
        .chars()
        .skip(entity.offset as usize)
        .take(entity.length as usize)
        .filter(|c| *c != '-')
        .collect();

    Some(code)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_get_code_block() {
        let entities = vec![tgbotapi::MessageEntity {
            entity_type: tgbotapi::MessageEntityType::Code,
            offset: 0,
            length: 36,
            url: None,
            user: None,
            language: None,
            custom_emoji_id: None,
        }];
        let text = "e52569fa-99a0-44fc-ae9d-2477177b550b";

        assert_eq!(
            Some("e52569fa99a044fcae9d2477177b550b".to_string()),
            super::get_code_block(&entities, &text)
        );

        let entities = vec![];
        assert_eq!(None, super::get_code_block(&entities, text));
    }
}
