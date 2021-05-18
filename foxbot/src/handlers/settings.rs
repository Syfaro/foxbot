use anyhow::Context;
use async_trait::async_trait;
use tgbotapi::{requests::*, *};

use super::{
    Handler,
    Status::{self, *},
};
use crate::MessageHandler;
use foxbot_models::{Sites, UserConfig, UserConfigKey};
use foxbot_utils::*;

pub struct SettingsHandler;

#[async_trait]
impl Handler for SettingsHandler {
    fn name(&self) -> &'static str {
        "settings"
    }

    async fn handle(
        &self,
        handler: &MessageHandler,
        update: &Update,
        command: Option<&Command>,
    ) -> anyhow::Result<Status> {
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

        Ok(Completed)
    }
}

async fn order(
    handler: &MessageHandler,
    callback_query: &CallbackQuery,
    data: &str,
) -> anyhow::Result<Status> {
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

        let order: Option<Vec<String>> = UserConfig::get(
            &handler.conn,
            UserConfigKey::SiteSortOrder,
            callback_query.from.id,
        )
        .await
        .context("unable to query user site sort order")?;
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
            &handler.conn,
            UserConfigKey::SiteSortOrder,
            callback_query.from.id,
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
    handler: &MessageHandler,
    message: &Message,
) -> anyhow::Result<Message> {
    let from = message
        .from
        .as_ref()
        .and_then(|user| user.language_code.as_deref());

    let site_preference = handler
        .get_fluent_bundle(from, |bundle| {
            get_message(&bundle, "settings-site-preference", None).unwrap()
        })
        .await;

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: site_preference,
            callback_data: Some("s:order:".into()),
            ..Default::default()
        }]],
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
    conn: &sqlx::Pool<sqlx::Postgres>,
    user_id: i64,
) -> anyhow::Result<InlineKeyboardMarkup> {
    let row: Option<Vec<String>> = UserConfig::get(&conn, UserConfigKey::SiteSortOrder, user_id)
        .await
        .context("unable to query user sort order")?;
    let mut sites = match row {
        Some(row) => row.iter().map(|item| item.parse().unwrap()).collect(),
        None => Sites::default_order(),
    };

    let mut buttons = vec![];

    // If the available sites has changed, reset ordering to add new items.
    if sites.len() != foxbot_models::Sites::len() {
        sites = foxbot_models::Sites::default_order();
        UserConfig::delete(&conn, UserConfigKey::SiteSortOrder, user_id).await?;
    }

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
