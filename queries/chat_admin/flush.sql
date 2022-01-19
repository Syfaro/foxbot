DELETE FROM
    chat_administrator
WHERE
    chat_id = lookup_chat_by_telegram_id($1)
    AND account_id <> lookup_account_by_telegram_id($2);
