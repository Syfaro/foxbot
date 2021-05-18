ALTER TABLE chat_administrator
    ALTER COLUMN account_id TYPE INTEGER,
    ALTER COLUMN chat_id TYPE INTEGER;

ALTER TABLE chat_telegram
    DROP CONSTRAINT chat_telegram_chat_id_fkey;
ALTER TABLE chat_telegram
    ADD FOREIGN KEY (chat_id) REFERENCES chat (id) ON DELETE CASCADE;
