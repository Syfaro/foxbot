use async_trait::async_trait;
use tgbotapi::*;

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_utils::*;

pub struct GroupSourceHandler;

#[async_trait]
impl Handler for GroupSourceHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<Status> {
        let message = needs_field!(update, message);
        needs_field!(message, photo);

        if matches!(message.via_bot, Some(tgbotapi::User { id, .. }) if id == handler.bot_user.id) {
            return Ok(Ignored);
        }

        tracing::debug!("passing channel photo to background worker");

        let custom = get_faktory_custom();

        let faktory = handler.faktory.clone();
        let message = message.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut faktory = faktory.lock().unwrap();
            let message = serde_json::to_value(&message).unwrap();
            let mut job =
                faktory::Job::new("group_photo", vec![message]).on_queue("foxbot_background");
            job.custom = custom;

            faktory.enqueue(job).unwrap();
        });

        Ok(Completed)
    }
}
