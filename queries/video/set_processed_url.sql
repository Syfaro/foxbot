UPDATE
    videos
SET
    processed = true,
    mp4_url = $2,
    thumb_url = $3
WHERE
    id = $1;
