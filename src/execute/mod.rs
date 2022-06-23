mod discord;
mod reddit;
mod telegram;

pub use discord::discord as start_discord;

pub use reddit::reddit as start_reddit;

pub use telegram::jobs as telegram_jobs;
pub use telegram::telegram as start_telegram;
