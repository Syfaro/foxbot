ALTER TABLE chat_administrator
    RENAME COLUMN last_update TO updated_at;
ALTER TABLE chat_administrator
    ALTER COLUMN updated_at SET DEFAULT current_timestamp,
    ALTER COLUMN updated_at SET NOT NULL;
ALTER TABLE chat_administrator
    ADD COLUMN id SERIAL NOT NULL;
ALTER TABLE chat_administrator
    DROP CONSTRAINT chat_administrator_pkey;
ALTER TABLE chat_administrator
    ADD PRIMARY KEY (id);
CREATE INDEX chat_administrator_updated_at_idx ON chat_administrator (updated_at);

ALTER TABLE group_config
    ADD COLUMN updated_at TIMESTAMP WITHOUT TIME ZONE NOT NULL DEFAULT current_timestamp;
DROP INDEX group_config_chat_id_name_idx;
CREATE INDEX group_config_updated_at_idx ON group_config (updated_at);
CREATE INDEX group_config_chat_id_name_idx ON group_config (chat_id, name);

ALTER TABLE permission
    ADD COLUMN id SERIAL NOT NULL;
ALTER TABLE permission
    DROP CONSTRAINT permission_pkey;
ALTER TABLE permission
    ADD PRIMARY KEY (id);
CREATE INDEX permission_updated_at_idx ON permission (updated_at);

ALTER TABLE user_config
    ADD COLUMN updated_at TIMESTAMP WITHOUT TIME ZONE NOT NULL DEFAULT current_timestamp;
DROP INDEX user_config_user_id_name_idx;
CREATE INDEX user_config_update_at_idx ON user_config (updated_at);
CREATE INDEX user_config_account_id_name_idx ON user_config (account_id, name);
