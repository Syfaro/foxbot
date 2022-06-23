SELECT
    exists(
        SELECT
            1
        FROM
            reddit_processed_mention
        WHERE
            mention_id = $1
    );
