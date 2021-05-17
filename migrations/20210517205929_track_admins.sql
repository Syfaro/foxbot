CREATE TABLE chat_administrator (
    account_id BIGINT REFERENCES account (id),
    chat_id BIGINT REFERENCES chat (id),

    is_admin BOOLEAN NOT NULL,
    last_update TIMESTAMP WITHOUT TIME ZONE NOT NULL,

    PRIMARY KEY (account_id, chat_id)
);
