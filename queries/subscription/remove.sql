DELETE FROM
    source_notification
WHERE
    account_id = lookup_account($1, $2)
    AND hash <@ ($3, 0);
