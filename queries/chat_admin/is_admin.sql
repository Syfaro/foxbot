SELECT
    is_admin
FROM
    chat_administrator
WHERE
    account_id = lookup_account($1, $2)
    AND chat_id = lookup_chat_by_telegram_id($3)
ORDER BY
    updated_at DESC
limit
    1;
