#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.create_table("cached_post", |t| {
        t.add_column("id", types::primary());
        t.add_column("post_url", types::varchar(512).nullable(false));
        t.add_column("thumb", types::boolean().nullable(false));
        t.add_column("cdn_url", types::varchar(512).nullable(false));
        t.add_column("width", types::integer().nullable(false));
        t.add_column("height", types::integer().nullable(false));

        t.add_index("cached_post_unique", types::index(vec!["post_url", "thumb"]).unique(true).nullable(false));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        CREATE TABLE cached_post (
            id SERIAL PRIMARY KEY,
            post_url TEXT NOT NULL,
            thumb BOOLEAN NOT NULL,
            cdn_url TEXT NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL
        );

        CREATE UNIQUE INDEX ON cached_post (post_url, thumb);
    ".to_string()
}
