CREATE TABLE reddit_processed_mention (
    mention_id text PRIMARY KEY,
    processed_at timestamp with time zone DEFAULT current_timestamp,
    perceptual_hash bigint,
    created_comment_id text NOT NULL UNIQUE
);
