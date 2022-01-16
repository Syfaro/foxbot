INSERT INTO
    chat_administrator (account_id, chat_id, is_admin, updated_at)
VALUES
    (
        lookup_account($1, $2),
        lookup_chat_by_telegram_id($3),
        $4,
        to_timestamp($5::bigint)
    );
