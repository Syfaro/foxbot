CREATE TABLE file_url_cache (
    id SERIAL PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    hash BIGINT
);

ALTER TABLE account
    ADD COLUMN discord_id NUMERIC UNIQUE;
ALTER TABLE account
    ALTER COLUMN telegram_id DROP NOT NULL;
ALTER TABLE account
    ADD CONSTRAINT service_id_exists_check CHECK (num_nonnulls(telegram_id, discord_id) >= 1);

CREATE FUNCTION lookup_account_by_discord_id(_discord_id NUMERIC)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _account_id INT;
BEGIN
    SELECT account.id FROM account
    INTO _account_id
    WHERE account.discord_id = _discord_id;

    IF NOT FOUND THEN
        INSERT INTO account (discord_id) VALUES (_discord_id) RETURNING id INTO _account_id;
    END IF;

    RETURN _account_id;
END;
$$;

CREATE FUNCTION lookup_account(_telegram_id BIGINT = NULL, _discord_id NUMERIC = NULL)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _account_id INT;
BEGIN
    IF num_nonnulls(_telegram_id, _discord_id) <> 1 THEN
        RAISE check_violation USING MESSAGE = 'Exactly one of Telegram and Discord IDs must be specified';
    END IF;

    IF _telegram_id IS NOT NULL THEN
        SELECT lookup_account_by_telegram_id(_telegram_id) INTO _account_id;
    ELSIF _discord_id IS NOT NULL THEN
        SELECT lookup_account_by_discord_id(_discord_id) INTO _account_id;
    END IF;

    RETURN _account_id;
END;
$$;

CREATE TABLE chat_discord (
    discord_id NUMERIC PRIMARY KEY,
    chat_id INTEGER NOT NULL REFERENCES chat (id)
);

CREATE FUNCTION lookup_chat_by_discord_id(_discord_id NUMERIC)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _chat_id INTEGER;
BEGIN
    SELECT chat_discord.chat_id FROM chat_discord
    INTO _chat_id
    WHERE chat_discord.discord_id = _discord_id;

    IF NOT FOUND THEN
        INSERT INTO chat DEFAULT VALUES RETURNING id INTO _chat_id;
        INSERT INTO chat_discord (discord_id, chat_id) VALUES (_discord_id, _chat_id);
    END IF;

    RETURN _chat_id;
END;
$$;

CREATE FUNCTION lookup_chat(_telegram_id BIGINT = NULL, _discord_id NUMERIC = NULL)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE _chat_id INT;
BEGIN
    IF num_nonnulls(_telegram_id, _discord_id) <> 1 THEN
        RAISE check_violation USING MESSAGE = 'Exactly one of Telegram and Discord IDs must be specified';
    END IF;

    IF _telegram_id IS NOT NULL THEN
        SELECT lookup_chat_by_telegram_id(_telegram_id) INTO _chat_id;
    ELSIF _discord_id IS NOT NULL THEN
        SELECT lookup_chat_by_discord_id(_discord_id) INTO _chat_id;
    END IF;

    RETURN _chat_id;
END;
$$;
