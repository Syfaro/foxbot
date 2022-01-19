INSERT INTO
    twitter_account (account_id, consumer_key, consumer_secret)
VALUES
    (lookup_account($1, $2), $3, $4) RETURNING id, consumer_key, consumer_secret;
