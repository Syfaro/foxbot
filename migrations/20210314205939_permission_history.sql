CREATE TABLE permission (
    chat_id BIGINT NOT NULL,
    updated_at TIMESTAMP WITHOUT TIME ZONE NOT NULL,

    permissions JSONB NOT NULL,

    PRIMARY KEY (chat_id, updated_at)
);
