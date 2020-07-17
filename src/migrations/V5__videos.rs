#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.create_table("videos", |t| {
        t.add_column("id", types::primary());
        t.add_column("processed", types::boolean().nullable(false).default(false));
        t.add_column("source", types::varchar(512).nullable(false));
        t.add_column("url", types::varchar(512).nullable(false).unique(true));
        t.add_column("mp4_url", types::varchar(512).nullable(true).unique(true));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        CREATE TABLE videos (
            id SERIAL PRIMARY KEY,
            processed BOOLEAN NOT NULL DEFAULT FALSE,
            source TEXT NOT NULL,
            url TEXT UNIQUE NOT NULL,
            mp4_url TEXT UNIQUE
        )
    ".to_string()
}
