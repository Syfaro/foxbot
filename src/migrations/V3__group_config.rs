#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.create_table("group_config", |t| {
        t.add_column("id", types::primary());
        t.add_column("chat_id", types::integer().nullable(false));
        t.add_column("name", types::varchar(255).nullable(false));
        t.add_column("value", types::varchar(255).nullable(false));

        t.add_index("group_config_lookup", types::index(vec!["chat_id", "name"]).unique(true).nullable(false));
    });

    m.create_table("user_group_config", |t| {
        t.add_column("id", types::primary());
        t.add_column("chat_id", types::integer().nullable(false));
        t.add_column("user_id", types::integer().nullable(false));
        t.add_column("name", types::varchar(255).nullable(false));
        t.add_column("value", types::varchar(255).nullable(false));

        t.add_index("user_group_config_lookup", types::index(vec!["chat_id", "user_id", "name"]).unique(true).nullable(false));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        CREATE TABLE group_config (
            id SERIAL PRIMARY KEY,
            chat_id BIGINT NOT NULL,
            name TEXT NOT NULL,
            value TEXT NOT NULL
        );

        CREATE UNIQUE INDEX ON group_config (chat_id, name);

        CREATE TABLE user_group_config (
            id SERIAL PRIMARY KEY,
            chat_id BIGINT NOT NULL,
            user_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            value TEXT NOT NULL
        );

        CREATE UNIQUE INDEX ON user_group_config (chat_id, user_id, name);
    ".to_string()
}
