use barrel::{types, Migration, backend::Sqlite};

pub fn migration() -> String {
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
