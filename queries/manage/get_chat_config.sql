SELECT
    DISTINCT ON (name) name,
    value
FROM
    group_config
WHERE
    chat_id = lookup_chat($1, $2)
ORDER BY
    name,
    updated_at DESC;
