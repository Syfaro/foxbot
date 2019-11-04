# foxbot

Telegram bot for easily collecting furry images from multiple sites.

Written as the successor of [furryimgbot](https://git.huefox.com/syfaro/telegram-furryimgbot).

It currently supports a number of sites:

* FurAffinity (including source finding via [fa.huefox.com](https://fa.huefox.com))
* Mastodon
* Weasyl
* direct links

## Configuration

Env Name            | Description
--------------------|------------
`FA_A`              | FurAffinity cookie 'a' from authenticated user
`FA_B`              | FurAffinity cookie 'b' from authenticated user
`FAUTIL_APITOKEN`   | API Token for [fa.huefox.com](https://fa.huefox.com/) to resolve FurAffinity direct links
`WEASYL_APITOKEN`   | API Token for [weasyl.com](https://www.weasyl.com)
`TELEGRAM_APITOKEN` | API Token for Telegram, from Botfather
