CREATE OR REPLACE FUNCTION lookup_account_by_telegram_id(_telegram_id BIGINT)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _account_id INT;
BEGIN
    SELECT account.id FROM account
    INTO _account_id
    WHERE account.telegram_id = _telegram_id;

    IF NOT FOUND THEN
        INSERT INTO account (telegram_id)
        VALUES (_telegram_id)
        ON CONFLICT DO NOTHING RETURNING id
        INTO _account_id;

        IF NOT FOUND THEN
            SELECT account.id FROM account
            INTO STRICT _account_id
            WHERE account.telegram_id = _telegram_id;
        END IF;
    END IF;

    RETURN _account_id;
END;
$$;

CREATE OR REPLACE FUNCTION lookup_chat_by_telegram_id(_telegram_id BIGINT)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _chat_id INTEGER;
BEGIN
    SELECT chat_telegram.chat_id FROM chat_telegram
    INTO _chat_id
    WHERE chat_telegram.telegram_id = _telegram_id;

    IF NOT FOUND THEN
        INSERT INTO chat DEFAULT VALUES RETURNING id INTO STRICT _chat_id;

        INSERT INTO chat_telegram (telegram_id, chat_id)
        VALUES (_telegram_id, _chat_id)
        ON CONFLICT DO NOTHING;

        IF NOT FOUND THEN
            DELETE FROM chat WHERE id = _chat_id;

            SELECT chat_telegram.chat_id FROM chat_telegram
            INTO STRICT _chat_id
            WHERE chat_telegram.telegram_id = _telegram_id;
        END IF;
    END IF;

    RETURN _chat_id;
END;
$$;
