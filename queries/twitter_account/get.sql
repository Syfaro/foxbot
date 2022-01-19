SELECT
    id,
    consumer_key,
    consumer_secret
FROM
    twitter_account
WHERE
    account_id = lookup_account($1, $2);
