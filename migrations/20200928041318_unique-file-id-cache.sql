CREATE TABLE file_id_cache (
    id SERIAL PRIMARY KEY,
    file_id TEXT UNIQUE NOT NULL,
    hash BIGINT NOT NULL
);
