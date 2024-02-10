WITH lookup_result AS (
    SELECT
        lookup_chat($1, $2) chat_id
)
SELECT
    DISTINCT ON (name) name,
    value
FROM
    lookup_result
    JOIN group_config ON group_config.chat_id = lookup_result.chat_id
ORDER BY
    name,
    updated_at DESC;
