# foxbot

Telegram bot for easily collecting furry images from multiple sites.

It also supports ways of sourcing images including through sending images directly to the bot, using commands in groups, automatically adding sources to images posted in groups, or editing messages in channels to include the source.

It currently supports a number of sites:

- FurAffinity (including source finding via [fuzzysearch.net](https://fuzzysearch.net))
- Mastodon
- Weasyl
- Twitter
- e621 (finds original link from direct image links)
- direct links

It also supports trying to reverse image search images sent directly using [fuzzysearch.net](https://fuzzysearch.net).

For more details, see the [blog post](https://syfaro.net/blog/foxbot/) explaining all the features.

## Configuration

All fields are required, unless otherwise specified.

| Env Name                   | Description                                                 |
| -------------------------- | ----------------------------------------------------------- |
| `FA_A`                     | FurAffinity cookie 'a' from authenticated user              |
| `FA_B`                     | FurAffinity cookie 'b' from authenticated user              |
| `WEASYL_APITOKEN`          | API Token for [weasyl.com](https://www.weasyl.com)          |
| `INKBUNNY_USERNAME`        | Username for [Inkbunny](https://inkbunny.net)               |
| `INKBUNNY_PASSWORD`        | Password for [Inkbunny](https://inkbunny.net)               |
| `E621_LOGIN`               | Username for [e621](https://e621.net)                       |
| `E621_API_KEY`             | API key for [e621](https://e621.net)                        |
| `FAUTIL_APITOKEN`          | API Token for [fuzzysearch.net](https://fuzzysearch.net)    |
| `TWITTER_CONSUMER_KEY`     | Twitter app consumer key                                    |
| `TWITTER_CONSUMER_KEY`     | Twitter app consumer secret                                 |
| `TWITTER_CALLBACK`         | Twitter callback URL for authentication                     |
| `JAEGER_COLLECTOR`         | Jaeger collector endpoint                                   |
| `SENTRY_DSN`               | Optional, Sentry DSN to report errors                       |
| `SENTRY_ORGANIZATION_SLUG` | Optional, Sentry organization slug for user error messages  |
| `SENTRY_PROJECT_SLUG`      | Optional, Sentry project slug for user error messages       |
| `TELEGRAM_APITOKEN`        | API Token for Telegram, from Botfather                      |
| `S3_ENDPOINT`              | Endpoint for S3 for cached images and video storage         |
| `S3_REGION`                | Region for S3                                               |
| `S3_TOKEN`                 | S3 access token                                             |
| `S3_SECRET`                | S3 secret token                                             |
| `S3_BUCKET`                | S3 bucket                                                   |
| `S3_URL`                   | URL to use for generating path to file in S3 bucket         |
| `B2_ACCOUNT_ID`            | Backblaze B2 account ID for encoded videos                  |
| `B2_APP_KEY`               | Backblaze B2 app key for encoded videos                     |
| `B2_BUCKET_ID`             | Backblaze B2 bucket ID for encoded videos                   |
| `COCONUT_APITOKEN`         | API token for [Coconut](https://coconut.co)                 |
| `COCONUT_SECRET`           | Secret for [Coconut](https://coconut.co) webhooks           |
| `CACHE_ALL_IMAGES`         | Optional, download and cache all inline images              |
| `REDIS_DSN`                | Redis connection URL                                        |
| `FAKTORY_URL`              | Faktory connection URL                                      |
| `DATABASE_URL`             | PostgreSQL connection URL                                   |
| `INTERNET_URL`             | URL base for all webhooks and served data                   |
| `INTERNAL_SECRET`          | Secret key to access health and metrics                     |
| `BACKGROUND_WORKERS`       | Optional, number of concurrent workers for background tasks |
