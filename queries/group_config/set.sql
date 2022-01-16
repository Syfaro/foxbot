INSERT INTO
    group_config (chat_id, name, value)
VALUES
    (lookup_chat($1, $2), $3, $4);
