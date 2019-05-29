#![allow(dead_code)]

mod sites;
mod telegram;

fn main() {
    pretty_env_logger::init();

    let telegram = telegram::Telegram::new(std::env::var("TELEGRAM_APITOKEN").unwrap());

    let mut update_req = telegram::GetUpdates::default();
    update_req.timeout = Some(30);

    loop {
        let updates = match telegram.make_request(&update_req) {
            Ok(updates) => updates,
            Err(e) => panic!(e),
        };

        for update in updates {
            if let Some(message) = update.message {
                let text = message.text.unwrap();
                println!(
                    "Got message from {} with contents: {}",
                    message.from.unwrap().first_name,
                    text
                );

                let new_message = telegram::SendMessage {
                    chat_id: message.chat.id,
                    text: format!("Your message was: {}", text),
                };

                telegram.make_request(&new_message).unwrap();
            }

            update_req.offset = Some(update.update_id + 1);
        }
    }

    // let mut sites: Vec<Box<dyn sites::Site>> = vec![];

    // sites.push(Box::new(sites::FurAffinity::new(Some((
    //     "".into(),
    //     "".into(),
    // )))));
    // sites.push(Box::new(sites::Direct {}));

    // let url: &'static str = "https://d.facdn.net/art/deadrussiansoul/1555431774/1555431774.deadrussiansoul_Скан_20190411__7_.png".into();

    // for site in sites {
    //     if site.is_supported(&url) {
    //         println!("{:?}", site.get_images(&url));
    //     }
    // }
}
