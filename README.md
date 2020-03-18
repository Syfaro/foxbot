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
`FAUTIL_APITOKEN`          | API Token for [fuzzysearch.net](https://fuzzysearch.net)
`WEASYL_APITOKEN`          | API Token for [weasyl.com](https://www.weasyl.com)
`TELEGRAM_APITOKEN`        | API Token for Telegram, from Botfather
`TWITTER_CONSUMER_KEY`     | Twitter app consumer key
`TWITTER_CONSUMER_KEY`     | Twitter app consumer secret
`USE_WEBHOOKS`             | If should configure and use webhooks instead of polling
`WEBHOOK_ENDPOINT`         | If using webhooks, endpoint to set with Telegram
`HTTP_HOST`                | If using webhooks, host to listen for updates on
`HTTP_SECRET`              | If using webhooks, secret endpoint to use for Telegram updates
`INFLUX_HOST`              | InfluxDB host, including scheme
`INFLUX_DB`                | InfluxDB database name
`INFLUX_USER`              | InfluxDB username
`INFLUX_PASS`              | InfluxDB password
`SENTRY_DSN`               | Sentry DSN to report errors
`SENTRY_ORGANIZATION_SLUG` | Sentry organization slug
`SENTRY_PROJECT_SLUG`      | Sentry project slug
`JAEGER_COLLECTOR`         | Jaeger collector endpoint
`DATABASE`                 | Path to SQLite database to store configuration and persistent cache
