INSERT INTO
    video_job_message (video_id, chat_id, message_id)
VALUES
    ($1, lookup_chat($2, $3), $4);
