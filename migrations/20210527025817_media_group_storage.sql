CREATE TABLE media_group (
    id SERIAL PRIMARY KEY,
    media_group_id TEXT NOT NULL,
    inserted_at TIMESTAMP WITH TIME ZONE NOT NULL,
    message JSONB NOT NULL,
    sources JSONB
);

CREATE INDEX media_group_media_group_id_idx ON media_group (media_group_id);
