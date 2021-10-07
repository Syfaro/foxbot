use anyhow::Context;
use async_trait::async_trait;
use redis::AsyncCommands;
use tgbotapi::{
    requests::{
        AnswerCallbackQuery, EditMessageReplyMarkup, EditMessageText, ReplyMarkup, SendMessage,
    },
    CallbackQuery, Command, InlineKeyboardButton, InlineKeyboardMarkup, Message, Update,
};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};
use crate::MessageHandler;
use foxbot_models::{Sites, UserConfig, UserConfigKey};
use foxbot_utils::{get_message, needs_field};

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
                send_settings_message(handler, update.message.as_ref().unwrap())
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
            return order(handler, callback_query, data).await;
        }

        if data.starts_with("s:inline-history:") {
            return inline_history(handler, callback_query, data).await;
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
                get_message(bundle, "settings-unsupported", None).unwrap()
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
                get_message(bundle, "settings-move-unable", Some(args)).unwrap()
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
                get_message(bundle, "settings-move-updated", Some(args)).unwrap()
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
            get_message(bundle, "settings-site-order", None).unwrap()
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

async fn inline_history_keyboard(
    handler: &MessageHandler,
    user: &tgbotapi::User,
    enable: bool,
) -> InlineKeyboardMarkup {
    let (message_key, data) = if enable {
        ("settings-inline-history-enable", "s:inline-history:e")
    } else {
        ("settings-inline-history-disable", "s:inline-history:d")
    };

    let message = handler
        .get_fluent_bundle(user.language_code.as_deref(), |bundle| {
            get_message(bundle, message_key, None).unwrap()
        })
        .await;

    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: message,
            callback_data: Some(data.to_string()),
            ..Default::default()
        }]],
    }
}

async fn inline_history(
    handler: &MessageHandler,
    callback_query: &CallbackQuery,
    data: &str,
) -> anyhow::Result<Status> {
    let reply_message = needs_field!(callback_query, message);

    let text = handler
        .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
            get_message(bundle, "settings-inline-history-description", None).unwrap()
        })
        .await;

    if data.ends_with(":e") || data.ends_with(":d") {
        let enabled = data.chars().last().unwrap_or_default() == 'e';
        let message_key = if enabled {
            "settings-inline-history-enabled"
        } else {
            "settings-inline-history-disabled"
        };
        let message = handler
            .get_fluent_bundle(callback_query.from.language_code.as_deref(), |bundle| {
                get_message(bundle, message_key, None).unwrap()
            })
            .await;

        UserConfig::set(
            &handler.conn,
            UserConfigKey::InlineHistory,
            callback_query.from.id,
            enabled,
        )
        .await?;

        if !enabled {
            let mut redis = handler.redis.clone();
            redis
                .del(format!("inline-history:{}", callback_query.from.id))
                .await?;
        }

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(message),
            ..Default::default()
        };

        let keyboard = inline_history_keyboard(handler, &callback_query.from, !enabled).await;

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
        .context("unable to edit message or answer callback query")?;
    } else {
        let enabled = UserConfig::get(
            &handler.conn,
            UserConfigKey::InlineHistory,
            callback_query.from.id,
        )
        .await?
        .unwrap_or(false);

        let keyboard = inline_history_keyboard(handler, &callback_query.from, !enabled).await;

        let edit_message = EditMessageText {
            message_id: Some(reply_message.message_id),
            chat_id: reply_message.chat_id(),
            text,
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
            ..Default::default()
        };

        handler.make_request(&edit_message).await?;
    }

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

    let (site_preference, inline_history) = handler
        .get_fluent_bundle(from, |bundle| {
            (
                get_message(bundle, "settings-site-preference", None).unwrap(),
                get_message(bundle, "settings-inline-history", None).unwrap(),
            )
        })
        .await;

    let keyboard = InlineKeyboardMarkup {
        inline_keyboard: vec![
            vec![InlineKeyboardButton {
                text: site_preference,
                callback_data: Some("s:order:".into()),
                ..Default::default()
            }],
            vec![InlineKeyboardButton {
                text: inline_history,
                callback_data: Some("s:inline-history:".into()),
                ..Default::default()
            }],
        ],
    };

    let text = handler
        .get_fluent_bundle(from, |bundle| {
            get_message(bundle, "settings-main", None).unwrap()
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
    let row: Option<Vec<String>> = UserConfig::get(conn, UserConfigKey::SiteSortOrder, user_id)
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
        UserConfig::delete(conn, UserConfigKey::SiteSortOrder, user_id).await?;
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
