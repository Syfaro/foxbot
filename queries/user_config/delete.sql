DELETE FROM
    user_config
WHERE
    account_id = lookup_account($1, $2)
    AND name = $3;
