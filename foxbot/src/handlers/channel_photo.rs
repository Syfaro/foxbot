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

        let faktory = handler.faktory.clone();
        let message = message.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut faktory = faktory.lock().unwrap();
            let message = serde_json::to_value(&message).unwrap();
            faktory
                .enqueue(
                    faktory::Job::new("foxbot_channel_update", vec![message])
                        .on_queue("foxbot_channel"),
                )
                .unwrap();
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
