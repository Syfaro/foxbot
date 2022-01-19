INSERT INTO
    cached_post (post_url, thumb, cdn_url, width, height)
VALUES
    ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING;
