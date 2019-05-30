#![allow(dead_code)]

mod sites;

fn main() {
    pretty_env_logger::init();

    let mut sites: Vec<Box<dyn sites::Site>> = vec![];
    sites.push(Box::new(sites::Direct {}));

    let telegram = telegram::Telegram::new(std::env::var("TELEGRAM_APITOKEN").unwrap());

    let mut update_req = telegram::GetUpdates::default();
    update_req.timeout = Some(30);

    loop {
        let updates = match telegram.make_request(&update_req) {
            Ok(updates) => updates,
            Err(e) => panic!(e),
        };

        for update in updates {
            if let Some(inline) = update.inline_query {
                println!("{:?}", inline);
            }

            update_req.offset = Some(update.update_id + 1);
        }
    }
}
