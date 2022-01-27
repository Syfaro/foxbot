INSERT INTO
    video (source, url, display_url, display_name)
VALUES
    ($1, $2, $3, $4) ON CONFLICT ON CONSTRAINT unique_source DO
UPDATE
SET
    source = EXCLUDED.source RETURNING display_name;
