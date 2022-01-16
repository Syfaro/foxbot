SELECT
    account.id,
    account.telegram_id,
    account.discord_id,
    request_key,
    request_secret
FROM
    twitter_auth
    JOIN account ON account.id = twitter_auth.account_id
WHERE
    request_key = $1;
