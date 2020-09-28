CREATE TABLE user_config (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    value JSONB NOT NULL
);

CREATE UNIQUE INDEX ON user_config (user_id, name);
