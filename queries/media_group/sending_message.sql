INSERT INTO
    media_group_sent (media_group_id)
VALUES
    ($1) ON CONFLICT DO NOTHING RETURNING media_group_id;
