TRUNCATE TABLE videos CASCADE;

ALTER TABLE
    videos RENAME TO video;

ALTER TABLE
    video
ADD
    COLUMN file_size integer,
ADD
    COLUMN height integer,
ADD
    COLUMN width integer,
ADD
    COLUMN duration integer;

ALTER TABLE
    video
ALTER COLUMN
    job_id TYPE text;
