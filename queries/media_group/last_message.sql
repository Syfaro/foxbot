SELECT
    max(inserted_at)
FROM
    media_group
WHERE
    media_group_id = $1;
