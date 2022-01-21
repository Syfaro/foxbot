SELECT
    DISTINCT ON (chat_administrator.chat_id) chat_administrator.chat_id,
    telegram_id "telegram_id!",
    is_admin
FROM
    chat_administrator
    JOIN chat_telegram ON chat_telegram.chat_id = chat_administrator.chat_id
WHERE
    account_id = lookup_account($1, $2)
    AND is_admin = true
    AND telegram_id IS NOT NULL
ORDER BY
    chat_administrator.chat_id,
    updated_at DESC;
