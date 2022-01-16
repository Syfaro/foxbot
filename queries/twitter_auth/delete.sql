DELETE FROM
    twitter_auth
WHERE
    account_id = lookup_account($1, $2);
