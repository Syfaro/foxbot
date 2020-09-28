CREATE TABLE twitter_account (
    id SERIAL PRIMARY KEY,
    user_id INTEGER UNIQUE,
    consumer_key TEXT NOT NULL,
    consumer_secret TEXT NOT NULL
);

CREATE TABLE twitter_auth (
    id SERIAL PRIMARY KEY,
    user_id INTEGER UNIQUE,
    request_key TEXT NOT NULL,
    request_secret TEXT NOT NULL
);
