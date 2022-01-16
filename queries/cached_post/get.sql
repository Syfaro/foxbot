SELECT
    id,
    post_url,
    thumb,
    cdn_url,
    width,
    height
FROM
    cached_post
WHERE
    post_url = $1
    AND thumb = $2;
