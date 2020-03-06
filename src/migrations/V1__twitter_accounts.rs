use barrel::{types, Migration, backend::Sqlite};

pub fn migration() -> String {
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
