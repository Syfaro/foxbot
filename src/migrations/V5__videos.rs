use barrel::{types, Migration, backend::Sqlite};

pub fn migration() -> String {
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
