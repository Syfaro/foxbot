use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::needs_field;
use crate::utils::get_message;

#[derive(Clone, Debug, PartialEq)]
pub enum Sites {
    FurAffinity,
    E621,
    Twitter,
}

impl Sites {
    pub fn as_str(&self) -> &'static str {
        match *self {
            Sites::FurAffinity => "FurAffinity",
            Sites::E621 => "e621",
            Sites::Twitter => "Twitter",
        }
    }

    pub fn from_str(s: &str) -> Sites {
        match s {
            "FurAffinity" => Sites::FurAffinity,
            "e621" => Sites::E621,
            "Twitter" => Sites::Twitter,
            _ => panic!("Invalid value"),
        }
    }
}

pub struct SettingsHandler;

#[async_trait]
impl super::Handler for SettingsHandler {
    fn name(&self) -> &'static str {
        "settings"
    }

    async fn handle(
        &self,
        handler: &crate::MessageHandler,
        update: &Update,
        command: Option<&Command>,
    ) -> failure::Fallible<super::Status> {
        if let Some(command) = command {
            if command.name == "/settings" {
                send_settings_message(&handler, &update.message.as_ref().unwrap()).await?;
                return Ok(Completed);
            }
        }

        let callback_query = needs_field!(update, callback_query);
        let data = needs_field!(callback_query, data);

        if !data.starts_with("s:") {
            return Ok(Ignored);
        }

        if data.starts_with("s:order:") {
            if data.ends_with(":e") {
                let answer = AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    text: Some("Sorry, not yet supported.".into()),
                    ..Default::default()
                };

                handler.bot.make_request(&answer).await?;

                return Ok(Completed);
            }

            if data.ends_with(":-") {
                let site = Sites::from_str(data.split(':').skip(2).next().unwrap());

                let answer = AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    text: Some(format!("Unable to move {} to that position", site.as_str()).into()),
                    ..Default::default()
                };

                handler.bot.make_request(&answer).await?;

                return Ok(Completed);
            }

            let reply_message = needs_field!(callback_query, message);
            let from = reply_message
                .from
                .as_ref()
                .and_then(|from| from.language_code.as_deref());

            let pos = data.split(':').skip(3).next();
            if let Some(pos) = pos {
                let site = Sites::from_str(data.split(':').skip(2).next().unwrap());
                let pos: usize = pos.parse().unwrap();

                use quaint::prelude::*;

                let conn = handler.conn.check_out().await?;

                let order = conn
                    .select(
                        Select::from_table("user_config").so_that(
                            "user_id"
                                .equals(callback_query.from.id)
                                .and("name".equals("site-sort-order")),
                        ),
                    )
                    .await?;

                let has_config = order.len() != 0;

                let mut sites = if !has_config {
                    vec![Sites::FurAffinity, Sites::E621, Sites::Twitter]
                } else {
                    let row = order.into_single()?;
                    let data = row["value"].as_str().unwrap();
                    let data: Vec<String> = serde_json::from_str(&data)?;
                    data.iter().map(|item| Sites::from_str(&item)).collect()
                };

                let mut existing_pos = None;
                for (idx, item) in sites.iter().enumerate() {
                    if item == &site {
                        existing_pos = Some(idx);
                    }
                }

                if let Some(pos) = existing_pos {
                    sites.remove(pos);
                }

                sites.insert(pos, site.clone());

                let data = serde_json::to_string(
                    &sites.iter().map(|site| site.as_str()).collect::<Vec<_>>(),
                )?;

                if !has_config {
                    conn.insert(
                        Insert::single_into("user_config")
                            .value("user_id", callback_query.from.id)
                            .value("name", "site-sort-order")
                            .value("value", data)
                            .build(),
                    )
                    .await?;
                } else {
                    conn.update(
                        Update::table("user_config")
                            .so_that("user_id".equals(callback_query.from.id))
                            .set("value", data),
                    )
                    .await?;
                }

                let answer = AnswerCallbackQuery {
                    callback_query_id: callback_query.id.clone(),
                    text: Some(format!("Updated position for {}", site.as_str()).into()),
                    ..Default::default()
                };

                let keyboard = sort_order_keyboard(&handler.conn, callback_query.from.id).await?;

                let edit_message = EditMessageReplyMarkup {
                    message_id: Some(reply_message.message_id),
                    chat_id: reply_message.chat_id(),
                    reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
                    ..Default::default()
                };

                futures::try_join!(
                    handler.bot.make_request(&edit_message),
                    handler.bot.make_request(&answer)
                )?;

                return Ok(Completed);
            }

            let text = handler
                .get_fluent_bundle(from, |bundle| {
                    get_message(&bundle, "settings-site-order", None).unwrap()
                })
                .await;

            let keyboard = sort_order_keyboard(&handler.conn, callback_query.from.id).await?;

            let edit_message = EditMessageText {
                message_id: Some(reply_message.message_id),
                chat_id: reply_message.chat_id(),
                text,
                reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
                ..Default::default()
            };

            let answer = AnswerCallbackQuery {
                callback_query_id: callback_query.id.clone(),
                ..Default::default()
            };

            futures::try_join!(
                handler.bot.make_request(&edit_message),
                handler.bot.make_request(&answer)
            )?;
        }

        Ok(Completed)
    }
}

async fn send_settings_message(
    handler: &crate::MessageHandler,
    message: &Message,
) -> failure::Fallible<Message> {
    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "Site Preference".into(),
            callback_data: Some("s:order:".into()),
            ..Default::default()
        }]],
    };

    let user = message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    let text = handler
        .get_fluent_bundle(user, |bundle| {
            get_message(&bundle, "settings-main", None).unwrap()
        })
        .await;

    let message = SendMessage {
        chat_id: message.chat_id(),
        text,
        reply_to_message_id: if message.chat.chat_type.is_group() {
            Some(message.message_id)
        } else {
            None
        },
        reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
        ..Default::default()
    };

    Ok(handler.bot.make_request(&message).await?)
}

async fn sort_order_keyboard(
    conn: &quaint::pooled::Quaint,
    user_id: i32,
) -> failure::Fallible<InlineKeyboardMarkup> {
    use quaint::prelude::*;

    let conn = conn.check_out().await?;

    let order = conn
        .select(
            Select::from_table("user_config").so_that(
                "user_id"
                    .equals(user_id)
                    .and("name".equals("site-sort-order")),
            ),
        )
        .await?;

    let sites = if order.len() == 0 {
        vec![Sites::FurAffinity, Sites::E621, Sites::Twitter]
    } else {
        let row = order.into_single()?;
        let data = row["value"].as_str().unwrap();
        let data: Vec<String> = serde_json::from_str(&data)?;
        data.iter().map(|item| Sites::from_str(&item)).collect()
    };

    let mut buttons = vec![];

    for (idx, site) in sites.iter().enumerate() {
        let up = if idx == 0 {
            format!("s:order:{}:-", site.as_str())
        } else {
            format!("s:order:{}:{}", site.as_str(), idx - 1)
        };

        let down = if idx == sites.len() - 1 {
            format!("s:order:{}:-", site.as_str())
        } else {
            format!("s:order:{}:{}", site.as_str(), idx + 1)
        };

        buttons.push(vec![
            InlineKeyboardButton {
                text: site.as_str().into(),
                callback_data: Some(format!("s:order:{}:e", site.as_str())),
                ..Default::default()
            },
            InlineKeyboardButton {
                text: "⬆".into(),
                callback_data: Some(up),
                ..Default::default()
            },
            InlineKeyboardButton {
                text: "⬇".into(),
                callback_data: Some(down),
                ..Default::default()
            },
        ]);
    }

    Ok(InlineKeyboardMarkup {
        inline_keyboard: buttons,
    })
}
