#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.change_table("videos", |t| {
        t.add_column("chat_id", types::integer().nullable(true));
    });

    m.change_table("videos", |t| {
        t.add_column("message_id", types::integer().nullable(true));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        ALTER TABLE videos ADD COLUMN chat_id BIGINT ADD COLUMN message_id INTEGER;
    ".to_string()
}
