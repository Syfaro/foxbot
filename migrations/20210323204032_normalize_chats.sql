-- Create new table for storing all known chats.
CREATE TABLE chat (
    id SERIAL PRIMARY KEY,
    telegram_id BIGINT NOT NULL UNIQUE
);

-- Insert known chat IDs into new table, ignoring if they already existed.
INSERT INTO chat (telegram_id)
    SELECT chat_id FROM group_config UNION
    SELECT chat_id FROM permission UNION
    SELECT chat_id FROM video_job_message
ON CONFLICT DO NOTHING;

-- Update existing tables to reference the new chat table.
UPDATE group_config
    SET chat_id = (SELECT id FROM chat WHERE telegram_id = group_config.chat_id);
UPDATE permission
    SET chat_id = (SELECT id FROM chat WHERE telegram_id = permission.chat_id);
UPDATE video_job_message
    SET chat_id = (SELECT id FROM chat WHERE telegram_id = video_job_message.chat_id);

-- Update existing tables to add foreign key constraints for new chat table.
ALTER TABLE group_config
    ADD FOREIGN KEY (chat_id) REFERENCES chat (id) ON DELETE CASCADE,
    ALTER COLUMN chat_id TYPE INTEGER;
ALTER TABLE permission
    ADD FOREIGN KEY (chat_id) REFERENCES chat (id) ON DELETE CASCADE,
    ALTER COLUMN chat_id TYPE INTEGER;
ALTER TABLE video_job_message
    ADD FOREIGN KEY (chat_id) REFERENCES chat (id) ON DELETE CASCADE,
    ALTER COLUMN chat_id TYPE INTEGER;

-- Repeat all steps for users.

CREATE TABLE account (
    id SERIAL PRIMARY KEY,
    telegram_id BIGINT NOT NULL UNIQUE
);

INSERT INTO account (telegram_id)
    SELECT user_id FROM twitter_account UNION
    SELECT user_id FROM twitter_auth UNION
    SELECT user_id FROM user_config
ON CONFLICT DO NOTHING;

UPDATE twitter_account
    SET user_id = (SELECT id FROM account WHERE telegram_id = twitter_account.user_id);
UPDATE twitter_auth
    SET user_id = (SELECT id FROM account WHERE telegram_id = twitter_auth.user_id);
UPDATE user_config
    SET user_id = (SELECT id FROM account WHERE telegram_id = user_config.user_id);

ALTER TABLE twitter_account
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
ALTER TABLE twitter_auth
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
ALTER TABLE user_config
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
