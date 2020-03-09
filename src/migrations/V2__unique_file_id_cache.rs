use barrel::{types, Migration, backend::Sqlite};

pub fn migration() -> String {
    let mut m = Migration::new();

    m.create_table("file_id_cache", |t| {
        t.add_column("id", types::primary());
        t.add_column("file_id", types::varchar(255).unique(true).nullable(false));
        t.add_column("hash", types::integer().nullable(false));
    });

    m.make::<Sqlite>()
}
