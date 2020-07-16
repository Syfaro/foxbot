use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::Status::*;
use crate::models::{Sites, UserConfig, UserConfigKey};
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
    ) -> anyhow::Result<super::Status> {
        if let Some(command) = command {
            if command.name == "/settings" {
                send_settings_message(&handler, &update.message.as_ref().unwrap())
                    .await
                    .context("unable to send settings message")?;
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
) -> anyhow::Result<super::Status> {
    let reply_message = needs_field!(callback_query, message);
    let from = reply_message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    if data.ends_with(":t") {
        let conn = handler
            .conn
            .check_out()
            .await
            .context("unable to check out database")?;

        let source_name: Option<bool> =
            UserConfig::get(&conn, UserConfigKey::SourceName, callback_query.from.id)
                .await
                .context("unable to query user source name setting")?;
        let existed = source_name.is_some();
        let source_name = source_name.unwrap_or(false);

        let source_name = !source_name;

        UserConfig::set(
            &conn,
            "source-name",
            callback_query.from.id,
            existed,
            source_name,
        )
        .await
        .context("unable to set user source name setting")?;

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
            handler.make_request(&answer),
            handler.make_request(&edit_message)
        )
        .context("unable to send answer or edit message")?;

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

    handler
        .make_request(&edit_message)
        .await
        .context("unable to send setting message")?;

    Ok(Completed)
}

async fn name_keyboard(
    handler: &crate::MessageHandler,
    from: &User,
) -> anyhow::Result<InlineKeyboardMarkup> {
    let conn = handler
        .conn
        .check_out()
        .await
        .context("unable to check out database")?;

    let enabled = UserConfig::get(&conn, UserConfigKey::SourceName, from.id)
        .await
        .context("unable to query user source setting")?
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
) -> anyhow::Result<super::Status> {
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

        handler
            .make_request(&answer)
            .await
            .context("unable to answer order callback query")?;

        return Ok(Completed);
    }

    if data.ends_with(":-") {
        let site: Sites = data.split(':').nth(2).unwrap().parse().unwrap();

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

        handler
            .make_request(&answer)
            .await
            .context("unable to answer order callback query")?;

        return Ok(Completed);
    }

    let reply_message = needs_field!(callback_query, message);
    let from = reply_message
        .from
        .as_ref()
        .and_then(|from| from.language_code.as_deref());

    let pos = data.split(':').nth(3);
    if let Some(pos) = pos {
        let site = data.split(':').nth(2).unwrap().parse().unwrap();
        let pos: usize = pos.parse().unwrap();

        let conn = handler
            .conn
            .check_out()
            .await
            .context("unable to check out database")?;

        let order: Option<Vec<String>> =
            UserConfig::get(&conn, UserConfigKey::SiteSortOrder, callback_query.from.id)
                .await
                .context("unable to query user site sort order")?;
        let has_config = order.is_some();
        let mut sites = match order {
            Some(sites) => sites.iter().map(|item| item.parse().unwrap()).collect(),
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

        UserConfig::set(
            &conn,
            "site-sort-order",
            callback_query.from.id,
            has_config,
            sites,
        )
        .await
        .context("unable to set user sort order")?;

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
            handler.make_request(&edit_message),
            handler.make_request(&answer)
        )
        .context("unable to edit message or answer query")?;

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
        handler.make_request(&edit_message),
        handler.make_request(&answer)
    )
    .context("unable to edit message or answer callback query")?;

    Ok(Completed)
}

async fn send_settings_message(
    handler: &crate::MessageHandler,
    message: &Message,
) -> anyhow::Result<Message> {
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

    let sent_message = handler
        .make_request(&message)
        .await
        .context("unable to send settings message")?;

    Ok(sent_message)
}

async fn sort_order_keyboard(
    conn: &quaint::pooled::Quaint,
    user_id: i32,
) -> anyhow::Result<InlineKeyboardMarkup> {
    let conn = conn
        .check_out()
        .await
        .context("unable to check out database")?;

    let row: Option<Vec<String>> = UserConfig::get(&conn, UserConfigKey::SiteSortOrder, user_id)
        .await
        .context("unable to query user sort order")?;
    let sites = match row {
        Some(row) => row.iter().map(|item| item.parse().unwrap()).collect(),
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
