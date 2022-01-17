mod discord;
mod telegram;

pub use discord::discord as start_discord;
pub use telegram::telegram as start_telegram;
