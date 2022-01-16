INSERT INTO
    user_config (account_id, name, value)
VALUES
    (lookup_account($1, $2), $3, $4);
