use async_trait::async_trait;
use sentry::integrations::failure::capture_fail;
use telegram::*;
pub struct ChosenInlineHandler;

#[async_trait]
impl crate::Handler for ChosenInlineHandler {
    fn name(&self) -> &'static str {
        "chosen"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: Update,
        _command: Option<Command>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let handled = if let Some(chosen_result) = update.chosen_inline_result {
            let point = influxdb::Query::write_query(influxdb::Timestamp::Now, "chosen")
                .add_field("user_id", chosen_result.from.id);

            if let Err(e) = handler.influx.query(&point).await {
                log::error!("Unable to send chosen inline result to InfluxDB: {:?}", e);
                capture_fail(&e);
            }

            true
        } else {
            false
        };

        Ok(handled)
    }
}
