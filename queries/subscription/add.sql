INSERT INTO
    source_notification (account_id, hash, message_id, photo_id)
VALUES
    (lookup_account($1, $2), $3, $4, $5) ON CONFLICT DO NOTHING;
