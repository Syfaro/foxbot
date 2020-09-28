CREATE TABLE group_config (
    id SERIAL PRIMARY KEY,
    chat_id BIGINT NOT NULL,
    name TEXT NOT NULL,
    value JSONB NOT NULL
);

CREATE UNIQUE INDEX ON group_config (chat_id, name);
