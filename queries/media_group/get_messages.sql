SELECT
    id,
    media_group_id,
    inserted_at,
    message "message: Json<Message>",
    sources "sources: Json<Vec<fuzzysearch::File>>"
FROM
    media_group
WHERE
    media_group_id = $1;
