# foxbot

Telegram bot for easily collecting furry images from multiple sites.

Written as the successor of [furryimgbot](https://git.huefox.com/syfaro/telegram-furryimgbot).

It currently supports a number of sites:

* FurAffinity (including source finding via [fa.huefox.com](https://fa.huefox.com))
* Mastodon
* Weasyl
* Twitter
* e621 (finds original link from direct image links)
* direct links

## Configuration

Env Name               | Description
-----------------------|------------
`FA_A`                 | FurAffinity cookie 'a' from authenticated user
`FA_B`                 | FurAffinity cookie 'b' from authenticated user
`FAUTIL_APITOKEN`      | API Token for [fa.huefox.com](https://fa.huefox.com/) to resolve FurAffinity direct links
`WEASYL_APITOKEN`      | API Token for [weasyl.com](https://www.weasyl.com)
`TELEGRAM_APITOKEN`    | API Token for Telegram, from Botfather
`TWITTER_CONSUMER_KEY` | Twitter app consumer key
`TWITTER_CONSUMER_KEY` | Twitter app consumer secret
`TWITTER_DATABASE`     | Path to database file to store Twitter credentials
`USE_WEBHOOKS`         | If should configure and use webhooks instead of polling
`WEBHOOK_ENDPOINT`     | If using webhooks, endpoint to set with Telegram
`HTTP_HOST`            | If using webhooks, host to listen for updates on
`HTTP_SECRET`          | If using webhooks, secret endpoint to use for Telegram updates
