UPDATE
    media_group
SET
    sources = $2
WHERE
    id = $1;
