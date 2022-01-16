SELECT
    value
FROM
    user_config
WHERE
    user_config.account_id = lookup_account($1, $2)
    AND name = $3
ORDER BY
    updated_at DESC
LIMIT
    1;
