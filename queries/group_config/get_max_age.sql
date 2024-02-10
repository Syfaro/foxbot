SELECT
    value
FROM
    group_config
WHERE
    chat_id = lookup_chat($1, $2)
    AND name = $3
    AND updated_at >= $4
ORDER BY
    updated_at DESC
LIMIT
    1;
