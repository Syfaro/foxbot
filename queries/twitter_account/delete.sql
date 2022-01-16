DELETE FROM
    twitter_account
WHERE
    account_id = lookup_account($1, $2);
