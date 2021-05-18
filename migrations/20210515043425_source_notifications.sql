CREATE EXTENSION bktree;

CREATE TABLE source_notification (
    user_id BIGINT NOT NULL,
    hash BIGINT NOT NULL,
    message_id INT,
    photo_id TEXT,

    PRIMARY KEY (user_id, hash)
);

CREATE INDEX bk_source_notification_idx ON source_notification USING spgist (hash bktree_ops);
