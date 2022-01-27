UPDATE
    video
SET
    processed = true,
    mp4_url = $2,
    thumb_url = $3,
    file_size = $4,
    height = $5,
    width = $6
WHERE
    id = $1;
