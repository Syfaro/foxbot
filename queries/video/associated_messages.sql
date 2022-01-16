SELECT
    message_id,
    (
        SELECT
            chat_telegram.telegram_id
        FROM
            chat_telegram
        WHERE
            chat_id = video_job_message.chat_id
        ORDER BY
            abs(chat_telegram.telegram_id) DESC
        LIMIT
            1
    ) as "chat_id!"
FROM
    video_job_message
WHERE
    video_id = $1;
