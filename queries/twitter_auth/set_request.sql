INSERT INTO
    twitter_auth (account_id, request_key, request_secret)
VALUES
    (lookup_account($1, $2), $3, $4);
