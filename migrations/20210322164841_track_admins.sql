CREATE TABLE chat_administrator (
    user_id BIGINT,
    chat_id BIGINT,

    is_admin BOOLEAN NOT NULL,
    last_update TIMESTAMP WITHOUT TIME ZONE NOT NULL,

    PRIMARY KEY (user_id, chat_id)
);
