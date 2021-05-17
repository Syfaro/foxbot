-- Create new table for storing all known chats.
CREATE TABLE chat (
    id SERIAL PRIMARY KEY
);

-- Create new table for associating a Telegram ID with a chat.
CREATE TABLE chat_telegram (
    telegram_id BIGINT PRIMARY KEY,
    chat_id INTEGER NOT NULL REFERENCES chat (id)
);

-- Helper function to get the correct chat ID for a Telegram ID.
CREATE FUNCTION lookup_chat_by_telegram_id(_telegram_id BIGINT)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _chat_id INTEGER;
BEGIN
    SELECT chat_telegram.chat_id FROM chat_telegram
    INTO _chat_id
    WHERE chat_telegram.telegram_id = _telegram_id;

    IF NOT FOUND THEN
        INSERT INTO chat DEFAULT VALUES RETURNING id INTO _chat_id;
        INSERT INTO chat_telegram (telegram_id, chat_id) VALUES (_telegram_id, _chat_id);
    END IF;

    RETURN _chat_id;
END;
$$;

-- Update existing tables to reference the new chat table.
UPDATE group_config SET chat_id = lookup_chat_by_telegram_id(group_config.chat_id);
UPDATE permission SET chat_id = lookup_chat_by_telegram_id(permission.chat_id);
UPDATE video_job_message SET chat_id = lookup_chat_by_telegram_id(video_job_message.chat_id);

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

CREATE FUNCTION lookup_account_by_telegram_id(_telegram_id BIGINT)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _account_id INT;
BEGIN
    SELECT account.id FROM account
    INTO _account_id
    WHERE account.telegram_id = _telegram_id;

    IF NOT FOUND THEN
        INSERT INTO account (telegram_id) VALUES (_telegram_id) RETURNING id INTO _account_id;
    END IF;

    RETURN _account_id;
END;
$$;

UPDATE twitter_account SET user_id = lookup_account_by_telegram_id(twitter_account.user_id);
UPDATE twitter_auth SET user_id = lookup_account_by_telegram_id(twitter_auth.user_id);
UPDATE user_config SET user_id = lookup_account_by_telegram_id(user_config.user_id);
UPDATE source_notification SET user_id = lookup_account_by_telegram_id(source_notification.user_id);

ALTER TABLE twitter_account
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
ALTER TABLE twitter_auth
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
ALTER TABLE user_config
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;
ALTER TABLE source_notification
    ADD FOREIGN KEY (user_id) REFERENCES account (id) ON DELETE CASCADE,
    ALTER COLUMN user_id TYPE INTEGER;

ALTER TABLE twitter_account RENAME COLUMN user_id TO account_id;
ALTER TABLE twitter_auth RENAME COLUMN user_id TO account_id;
ALTER TABLE user_config RENAME COLUMN user_id TO account_id;
ALTER TABLE source_notification RENAME COLUMN user_id TO account_id;
