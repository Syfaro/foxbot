use barrel::{types, Migration, backend::Sqlite};

pub fn migration() -> String {
    let mut m = Migration::new();

    m.create_table("user_config", |t| {
        t.add_column("id", types::primary());
        t.add_column("user_id", types::integer().nullable(false));
        t.add_column("name", types::varchar(255).nullable(false));
        t.add_column("value", types::varchar(255).nullable(false));

        t.add_index("user_config_lookup", types::index(vec!["user_id", "name"]).unique(true).nullable(false));
    });

    m.make::<Sqlite>()
}
