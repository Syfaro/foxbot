CREATE TABLE cached_post (
    id SERIAL PRIMARY KEY,
    post_url TEXT NOT NULL,
    thumb BOOLEAN NOT NULL,
    cdn_url TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL
);

CREATE UNIQUE INDEX ON cached_post (post_url, thumb);
