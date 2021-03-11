# foxbot

Telegram bot for easily collecting furry images from multiple sites.

It also supports ways of sourcing images including through sending images directly to the bot, using commands in groups, automatically adding sources to images posted in groups, or editing messages in channels to include the source.

Written as the successor of [furryimgbot](https://git.huefox.com/syfaro/telegram-furryimgbot).

It currently supports a number of sites:

* FurAffinity (including source finding via [fuzzysearch.net](https://fuzzysearch.net))
* Mastodon
* Weasyl
* Twitter
* e621 (finds original link from direct image links)
* direct links

It also supports trying to reverse image search images sent directly using [fuzzysearch.net](https://fuzzysearch.net).

## Configuration

Env Name                   | Description
---------------------------|------------
`FA_A`                     | FurAffinity cookie 'a' from authenticated user
`FA_B`                     | FurAffinity cookie 'b' from authenticated user
`WEASYL_APITOKEN`          | API Token for [weasyl.com](https://www.weasyl.com)
`INKBUNNY_USERNAME`        | Username for [Inkbunny](https://inkbunny.net)
`INKBUNNY_PASSWORD`        | Password for [Inkbunny](https://inkbunny.net)
`TWITTER_CONSUMER_KEY`     | Twitter app consumer key
`TWITTER_CONSUMER_KEY`     | Twitter app consumer secret
`TWITTER_CALLBACK`         | Twitter callback URL for authentication
`JAEGER_COLLECTOR`         | Jaeger collector endpoint
`SENTRY_DSN`               | Optional, Sentry DSN to report errors
`SENTRY_ORGANIZATION_SLUG` | Optional, Sentry organization slug for user error messages
`SENTRY_PROJECT_SLUG`      | Optional, Sentry project slug for user error messages
`TELEGRAM_APITOKEN`        | API Token for Telegram, from Botfather
`USE_WEBHOOKS`             | Optional, if should configure and use webhooks instead of polling
`WEBHOOK_ENDPOINT`         | Optional, if using webhooks, endpoint to set with Telegram
`HTTP_HOST`                | Optional, host to listen for webhooks
`HTTP_SECRET`              | Optional, if using webhooks, secret endpoint to use for Telegram updates
`S3_ENDPOINT`              | Endpoint for S3 for cached images and video storage
`S3_REGION`                | Region for S3
`S3_TOKEN`                 | S3 access token
`S3_SECRET`                | S3 secret token
`S3_BUCKET`                | S3 bucket
`S3_URL`                   | URL to use for generating path to file in S3 bucket
`FAUTIL_APITOKEN`          | API Token for [fuzzysearch.net](https://fuzzysearch.net)
`CACHE_ALL_IMAGES`         | Optional, download and cache all inline images
`METRICS_HOST`             | Host to expose Prometheus metrics, health check
`DB_HOST`                  | Host for PostgreSQL database
`DB_USER`                  | User for PostgreSQL database
`DB_PASS`                  | Password for PostgreSQL database
`DB_NAME`                  | Name of PostgreSQL database
