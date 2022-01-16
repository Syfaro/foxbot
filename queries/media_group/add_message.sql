INSERT INTO
    media_group (media_group_id, inserted_at, message)
VALUES
    ($1, $2, $3) RETURNING id;
