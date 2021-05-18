CREATE TABLE file_url_cache (
    id SERIAL PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    hash BIGINT NOT NULL
);
