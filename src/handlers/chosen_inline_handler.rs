use super::Status::*;
use async_trait::async_trait;
use telegram::*;

pub struct ChosenInlineHandler;

#[async_trait]
impl super::Handler for ChosenInlineHandler {
    fn name(&self) -> &'static str {
        "chosen"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> Result<super::Status, failure::Error> {
        Ok(if let Some(chosen_result) = &update.chosen_inline_result {
            let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "chosen")
                .add_field("user_id", chosen_result.from.id);

            let _ = handler.influx.query(&point).await;

            Completed
        } else {
            Ignored
        })
    }
}
