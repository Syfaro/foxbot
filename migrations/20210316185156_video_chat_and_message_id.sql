TRUNCATE TABLE videos;

ALTER TABLE videos
    ADD COLUMN job_id INTEGER,
    ADD COLUMN display_name TEXT NOT NULL UNIQUE,
    ADD COLUMN thumb_url TEXT,
    ADD COLUMN display_url TEXT NOT NULL,
    ADD COLUMN created_at TIMESTAMP WITHOUT TIME ZONE NOT NULL DEFAULT current_timestamp,
    ADD CONSTRAINT unique_source UNIQUE (source);

CREATE TABLE video_job_message (
    video_id INTEGER REFERENCES videos (id),

    chat_id BIGINT,
    message_id INTEGER,

    PRIMARY KEY (video_id, chat_id, message_id)
);
