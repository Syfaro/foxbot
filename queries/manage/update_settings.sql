INSERT INTO
    group_config (chat_id, name, value)
SELECT
    (lookup_chat($1, $2)) chat_id,
    setting.name,
    setting.value
FROM
    json_to_recordset($3::json) AS setting(name text, value json);
