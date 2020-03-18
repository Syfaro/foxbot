use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::models::{get_user_config, set_user_config, Sites};
use crate::needs_field;
use crate::utils::get_message;

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
            return order(&handler, &callback_query, &data).await;
        }

        if data.starts_with("s:name:") {
            return name(&handler, &callback_query, &data).await;
        }

        Ok(Completed)
    }
}

async fn name(
    handler: &crate::MessageHandler,
    callback_query: &CallbackQuery,
    data: &str,
) -> failure::Fallible<super::Status> {
    let reply_message = needs_field!(callback_query, message);
    let from = reply_message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    if data.ends_with(":t") {
        let conn = handler.conn.check_out().await?;

        let source_name: Option<bool> =
            get_user_config(&conn, "source-name", callback_query.from.id).await?;
        let existed = source_name.is_some();
        let source_name = source_name.unwrap_or(false);

        let source_name = !source_name;

        set_user_config(
            &conn,
            "source-name",
            callback_query.from.id,
            existed,
            source_name,
        )
        .await?;

        let text = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "settings-name-toggled", None).unwrap()
            })
            .await;

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        let keyboard = name_keyboard(&handler, &callback_query.from).await?;

        let edit_message = EditMessageReplyMarkup {
            message_id: Some(reply_message.message_id),
            chat_id: reply_message.chat_id(),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
            ..Default::default()
        };

        futures::try_join!(
            handler.bot.make_request(&answer),
            handler.bot.make_request(&edit_message)
        )?;

        return Ok(Completed);
    }

    let text = handler
        .get_fluent_bundle(from, |bundle| {
            get_message(&bundle, "settings-name", None).unwrap()
        })
        .await;

    let keyboard = name_keyboard(&handler, &callback_query.from).await?;

    let edit_message = EditMessageText {
        message_id: Some(reply_message.message_id),
        chat_id: reply_message.chat_id(),
        reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
        text,
        ..Default::default()
    };

    handler.bot.make_request(&edit_message).await?;

    Ok(Completed)
}

async fn name_keyboard(
    handler: &crate::MessageHandler,
    from: &User,
) -> failure::Fallible<InlineKeyboardMarkup> {
    let conn = handler.conn.check_out().await?;

    let enabled = get_user_config(&conn, "source-name", from.id)
        .await?
        .unwrap_or(false);

    let message_name = if enabled {
        "settings-name-source"
    } else {
        "settings-name-site"
    };

    let text = handler
        .get_fluent_bundle(from.language_code.as_deref(), |bundle| {
            get_message(&bundle, &message_name, None).unwrap()
        })
        .await;

    let keyboard = vec![vec![InlineKeyboardButton {
        text,
        callback_data: Some("s:name:t".into()),
        ..Default::default()
    }]];

    Ok(InlineKeyboardMarkup {
        inline_keyboard: keyboard,
    })
}

async fn order(
    handler: &crate::MessageHandler,
    callback_query: &CallbackQuery,
    data: &str,
) -> failure::Fallible<super::Status> {
    if data.ends_with(":e") {
        let text = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "settings-unsupported", None).unwrap()
            })
            .await;

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        handler.bot.make_request(&answer).await?;

        return Ok(Completed);
    }

    if data.ends_with(":-") {
        let site = Sites::from_str(data.split(':').nth(2).unwrap());

        let mut args = fluent::FluentArgs::new();
        args.insert("name", site.as_str().into());

        let text = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "settings-move-unable", Some(args)).unwrap()
            })
            .await;

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
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

    let pos = data.split(':').nth(3);
    if let Some(pos) = pos {
        let site = Sites::from_str(data.split(':').nth(2).unwrap());
        let pos: usize = pos.parse().unwrap();

        let conn = handler.conn.check_out().await?;

        let order: Option<Vec<String>> =
            get_user_config(&conn, "site-sort-order", callback_query.from.id).await?;
        let has_config = order.is_some();
        let mut sites = match order {
            Some(sites) => sites.iter().map(|item| Sites::from_str(&item)).collect(),
            None => Sites::default_order(),
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

        set_user_config(
            &conn,
            "site-sort-order",
            callback_query.from.id,
            has_config,
            sites,
        )
        .await?;

        let mut args = fluent::FluentArgs::new();
        args.insert("name", site.as_str().into());

        let text = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(&bundle, "settings-move-updated", Some(args)).unwrap()
            })
            .await;

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
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

    Ok(Completed)
}

async fn send_settings_message(
    handler: &crate::MessageHandler,
    message: &Message,
) -> failure::Fallible<Message> {
    let from = message
        .from
        .as_ref()
        .and_then(|user| user.language_code.as_deref());

    let (site_preference, source_name) = handler
        .get_fluent_bundle(from, |bundle| {
            (
                get_message(&bundle, "settings-site-preference", None).unwrap(),
                get_message(&bundle, "settings-source-name", None).unwrap(),
            )
        })
        .await;

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            InlineKeyboardButton {
                text: site_preference,
                callback_data: Some("s:order:".into()),
                ..Default::default()
            },
            InlineKeyboardButton {
                text: source_name,
                callback_data: Some("s:name:".into()),
                ..Default::default()
            },
        ]],
    };

    let text = handler
        .get_fluent_bundle(from, |bundle| {
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
    let conn = conn.check_out().await?;

    let row: Option<Vec<String>> = get_user_config(&conn, "site-sort-order", user_id).await?;
    let sites = match row {
        Some(row) => row.iter().map(|item| Sites::from_str(&item)).collect(),
        None => Sites::default_order(),
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
