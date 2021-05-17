use async_trait::async_trait;
use tgbotapi::*;

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_utils::*;

pub struct ChannelPhotoHandler;

#[async_trait]
impl Handler for ChannelPhotoHandler {
    fn name(&self) -> &'static str {
        "channel"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        // Ensure we have a channel_post Message and a photo within.
        let message = needs_field!(update, channel_post);
        needs_field!(&message, photo);

        potential_return!(initial_filter(&message));

        let custom = get_faktory_custom();

        let faktory = handler.faktory.clone();
        let message = message.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut faktory = faktory.lock().unwrap();
            let message = serde_json::to_value(&message).unwrap();
            let mut job =
                faktory::Job::new("channel_update", vec![message]).on_queue("foxbot_background");
            job.custom = custom;

            faktory.enqueue(job).unwrap();
        });

        Ok(Completed)
    }
}

/// Filter updates to ignore any non-channel type messages and flag completed
/// for forwarded messages (can't edit) or messages with reply markup
/// (likely from a bot and unable to be edited).
#[allow(clippy::unnecessary_wraps)]
fn initial_filter(message: &tgbotapi::Message) -> anyhow::Result<Option<Status>> {
    // We only want messages from channels. I think this is always true
    // because this came from a channel_post.
    if message.chat.chat_type != ChatType::Channel {
        return Ok(Some(Ignored));
    }

    // We can't edit forwarded messages, so we have to ignore.
    if message.forward_date.is_some() {
        return Ok(Some(Completed));
    }

    // See comment on [`check_response`], but this likely means we can't edit
    // this message. Might be worth testing more in the future, but for now, we
    // should just ignore it.
    if message.reply_markup.is_some() {
        return Ok(Some(Completed));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_initial_filter() {
        use super::initial_filter;
        use crate::handlers::Status::*;

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Private,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Ignored),
            "should filter out non-channel updates"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            forward_date: Some(0),
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Completed),
            "should mark forwarded messages as complete"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            reply_markup: Some(tgbotapi::InlineKeyboardMarkup {
                inline_keyboard: Default::default(),
            }),
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            Some(Completed),
            "should mark messages with reply markup as complete"
        );

        let message = tgbotapi::Message {
            chat: tgbotapi::Chat {
                chat_type: tgbotapi::ChatType::Channel,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            initial_filter(&message).unwrap(),
            None,
            "should not mark typical channel messages"
        );
    }
}
