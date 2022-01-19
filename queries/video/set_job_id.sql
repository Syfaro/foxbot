UPDATE
    videos
SET
    job_id = $2
WHERE
    id = $1;
