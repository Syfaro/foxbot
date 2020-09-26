use super::Status::*;
use crate::needs_field;
use async_trait::async_trait;
use tgbotapi::*;

lazy_static::lazy_static! {
    static ref CHOSEN_COUNTER: prometheus::Counter = prometheus::register_counter!("foxbot_chosen_inline_total", "Total number of chosen inline results").unwrap();
}

pub struct ChosenInlineHandler;

#[async_trait]
impl super::Handler for ChosenInlineHandler {
    fn name(&self) -> &'static str {
        "chosen"
    }

    async fn handle(
        &self,
        _handler: &crate::MessageHandler,
        update: &Update,
        _command: Option<&Command>,
    ) -> anyhow::Result<super::Status> {
        let _chosen_result = needs_field!(update, chosen_inline_result);

        CHOSEN_COUNTER.inc();

        Ok(Completed)
    }
}
