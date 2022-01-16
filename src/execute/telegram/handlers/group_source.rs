use async_trait::async_trait;
use tgbotapi::{Command, Update};

use crate::{execute::telegram::Context, needs_field, serialize_args, Error};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct GroupSourceHandler;

#[async_trait]
impl Handler for GroupSourceHandler {
    fn name(&self) -> &'static str {
        "group"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<Status, Error> {
        let message = needs_field!(update, message);
        needs_field!(message, photo);

        if matches!(message.via_bot, Some(tgbotapi::User { id, .. }) if id == cx.bot_user.id) {
            return Ok(Ignored);
        }

        tracing::debug!("passing group photo to background worker");

        let job = faktory::Job::new("group_photo", serialize_args!(message))
            .on_queue(crate::web::FOXBOT_DEFAULT_QUEUE);
        cx.faktory.enqueue_job(job, None).await?;

        Ok(Completed)
    }
}
