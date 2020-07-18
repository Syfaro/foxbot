#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.create_table("file_id_cache", |t| {
        t.add_column("id", types::primary());
        t.add_column("file_id", types::varchar(255).unique(true).nullable(false));
        t.add_column("hash", types::integer().nullable(false));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        CREATE TABLE file_id_cache (
            id SERIAL PRIMARY KEY,
            file_id TEXT UNIQUE NOT NULL,
            hash BIGINT NOT NULL
        );
    ".to_string()
}
