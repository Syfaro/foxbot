#[cfg(feature = "sqlite")]
pub fn migration() -> String {
    use barrel::{types, Migration, backend::Sqlite};

    let mut m = Migration::new();

    m.create_table("twitter_account", |t| {
        t.add_column("id", types::primary());
        t.add_column("user_id", types::integer().unique(true));
        t.add_column("consumer_key", types::varchar(255).nullable(false));
        t.add_column("consumer_secret", types::varchar(255).nullable(false));
    });

    m.create_table("twitter_auth", |t| {
        t.add_column("id", types::primary());
        t.add_column("user_id", types::integer().unique(true));
        t.add_column("request_key", types::varchar(255).nullable(false));
        t.add_column("request_secret", types::varchar(255).nullable(false));
    });

    m.make::<Sqlite>()
}

#[cfg(feature = "postgres")]
pub fn migration() -> String {
    "
        CREATE TABLE twitter_account (
            id SERIAL PRIMARY KEY,
            user_id INTEGER UNIQUE,
            consumer_key TEXT NOT NULL,
            consumer_secret TEXT NOT NULL
        );

        CREATE TABLE twitter_auth (
            id SERIAL PRIMARY KEY,
            user_id INTEGER UNIQUE,
            request_key TEXT NOT NULL,
            request_secret TEXT NOT NULL
        );
    ".to_string()
}
