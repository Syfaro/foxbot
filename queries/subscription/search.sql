SELECT
    account.telegram_id,
    hash "hash!",
    message_id,
    photo_id
FROM
    source_notification
    JOIN account ON account.id = source_notification.account_id
WHERE
    hash <@ ($1, 3);
