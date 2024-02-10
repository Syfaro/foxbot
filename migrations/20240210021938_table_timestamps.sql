ALTER TABLE
    cached_post
ADD
    COLUMN created_at TIMESTAMP WITH TIME ZONE DEFAULT current_timestamp;

ALTER TABLE
    media_group_sent
ADD
    COLUMN created_at TIMESTAMP WITH TIME ZONE DEFAULT current_timestamp;

ALTER TABLE
    source_notification
ADD
    COLUMN created_at TIMESTAMP WITH TIME ZONE DEFAULT current_timestamp;

ALTER TABLE
    video_job_message
ADD
    COLUMN created_at TIMESTAMP WITH TIME ZONE DEFAULT current_timestamp;
