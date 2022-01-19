use async_trait::async_trait;
use tgbotapi::{ChatType, Command, Update};

use crate::{execute::telegram::jobs::ChannelUpdateJob, needs_field, potential_return, Error};

use super::{
    Context, Handler,
    Status::{self, *},
};

pub struct ChannelPhotoHandler;

#[async_trait]
impl Handler for ChannelPhotoHandler {
    fn name(&self) -> &'static str {
        "channel"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        // Ensure we have a channel_post Message and a photo within.
        let message = needs_field!(update, channel_post);
        needs_field!(&message, photo);

        potential_return!(initial_filter(message));

        let job = ChannelUpdateJob { message };

        cx.faktory.enqueue_job(job, None).await?;

        Ok(Completed)
    }
}

/// Filter updates to ignore any non-channel type messages and flag completed
/// for forwarded messages (can't edit) or messages with reply markup
/// (likely from a bot and unable to be edited).
#[allow(clippy::unnecessary_wraps)]
fn initial_filter(message: &tgbotapi::Message) -> Result<Option<Status>, Error> {
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
        use crate::execute::telegram::handlers::Status::*;

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
