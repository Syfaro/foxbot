use sqlx::PgPool;

use crate::sites;

mod discord;
mod telegram;

pub use discord::discord as start_discord;
pub use telegram::telegram as start_telegram;

async fn get_sites(pool: &PgPool, config: &crate::RunConfig) -> Vec<sites::BoxedSite> {
    sites::get_all_sites(
        config.furaffinity_cookie_a.clone(),
        config.furaffinity_cookie_b.clone(),
        config.fuzzysearch_api_token.clone(),
        config.weasyl_api_token.clone(),
        config.twitter_consumer_key.clone(),
        config.twitter_consumer_secret.clone(),
        config.inkbunny_username.clone(),
        config.inkbunny_password.clone(),
        config.e621_login.clone(),
        config.e621_api_token.clone(),
        pool.clone(),
    )
    .await
}
