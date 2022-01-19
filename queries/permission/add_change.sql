INSERT INTO
    permission (chat_id, updated_at, permissions)
VALUES
    (
        lookup_chat_by_telegram_id($1),
        to_timestamp($2::int),
        $3
    );
