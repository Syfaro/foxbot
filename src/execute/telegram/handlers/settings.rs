use async_trait::async_trait;
use fluent_bundle::FluentArgs;
use redis::AsyncCommands;
use tgbotapi::{
    requests::{
        AnswerCallbackQuery, EditMessageReplyMarkup, EditMessageText, ReplyMarkup, SendMessage,
    },
    CallbackQuery, Command, InlineKeyboardButton, InlineKeyboardMarkup, Message, Update,
};

use crate::{
    execute::telegram::Context,
    models::{self, User},
    needs_field, utils, Error,
};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct SettingsHandler;

#[async_trait]
impl Handler for SettingsHandler {
    fn name(&self) -> &'static str {
        "settings"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &Update,
        command: Option<&Command>,
    ) -> Result<Status, Error> {
        if let Some(command) = command {
            if command.name == "/settings" {
                send_settings_message(cx, update.message.as_ref().unwrap()).await?;
                return Ok(Completed);
            }
        }

        let callback_query = needs_field!(update, callback_query);
        let data = needs_field!(callback_query, data);

        if !data.starts_with("s:") {
            return Ok(Ignored);
        }

        if data.starts_with("s:order:") {
            return order(cx, callback_query, data).await;
        }

        if data.starts_with("s:inline-history:") {
            return inline_history(cx, callback_query, data).await;
        }

        Ok(Completed)
    }
}

async fn order(cx: &Context, callback_query: &CallbackQuery, data: &str) -> Result<Status, Error> {
    let bundle = cx
        .get_fluent_bundle(callback_query.from.language_code.as_deref())
        .await;

    if data.ends_with(":e") {
        let text = utils::get_message(&bundle, "settings-unsupported", None);

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        cx.make_request(&answer).await?;

        return Ok(Completed);
    }

    if data.ends_with(":-") {
        let site: models::Sites = data.split(':').nth(2).unwrap().parse().unwrap();

        let mut args = FluentArgs::new();
        args.set("name", site.as_str());

        let text = utils::get_message(&bundle, "settings-move-unable", Some(args));

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        cx.make_request(&answer).await?;

        return Ok(Completed);
    }

    let reply_message = needs_field!(callback_query, message);

    let pos = data.split(':').nth(3);
    if let Some(pos) = pos {
        let site = data.split(':').nth(2).unwrap().parse().unwrap();
        let pos: usize = pos.parse().unwrap();

        let order: Option<Vec<String>> = models::UserConfig::get(
            &cx.pool,
            models::UserConfigKey::SiteSortOrder,
            &callback_query.from,
        )
        .await?;
        let mut sites = match order {
            Some(sites) => sites.iter().map(|item| item.parse().unwrap()).collect(),
            None => models::Sites::default_order(),
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

        models::UserConfig::set(
            &cx.pool,
            models::UserConfigKey::SiteSortOrder,
            &callback_query.from,
            sites,
        )
        .await?;

        let mut args = FluentArgs::new();
        args.set("name", site.as_str());

        let text = utils::get_message(&bundle, "settings-move-updated", Some(args));

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(text),
            ..Default::default()
        };

        let keyboard = sort_order_keyboard(&cx.pool, &callback_query.from).await?;

        let edit_message = EditMessageReplyMarkup {
            message_id: Some(reply_message.message_id),
            chat_id: reply_message.chat_id(),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
            ..Default::default()
        };

        futures::try_join!(cx.make_request(&edit_message), cx.make_request(&answer))?;

        return Ok(Completed);
    }

    let text = utils::get_message(&bundle, "settings-site-order", None);

    let keyboard = sort_order_keyboard(&cx.pool, &callback_query.from).await?;

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

    futures::try_join!(cx.make_request(&edit_message), cx.make_request(&answer))?;

    Ok(Completed)
}

async fn inline_history_keyboard(
    cx: &Context,
    user: &tgbotapi::User,
    enable: bool,
) -> InlineKeyboardMarkup {
    let (message_key, data) = if enable {
        ("settings-inline-history-enable", "s:inline-history:e")
    } else {
        ("settings-inline-history-disable", "s:inline-history:d")
    };

    let bundle = cx.get_fluent_bundle(user.language_code.as_deref()).await;

    let message = utils::get_message(&bundle, message_key, None);

    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: message,
            callback_data: Some(data.to_string()),
            ..Default::default()
        }]],
    }
}

async fn inline_history(
    cx: &Context,
    callback_query: &CallbackQuery,
    data: &str,
) -> Result<Status, Error> {
    let reply_message = needs_field!(callback_query, message);

    let bundle = cx
        .get_fluent_bundle(callback_query.from.language_code.as_deref())
        .await;

    let text = utils::get_message(&bundle, "settings-inline-history-description", None);

    if data.ends_with(":e") || data.ends_with(":d") {
        let enabled = data.chars().last().unwrap_or_default() == 'e';
        let message_key = if enabled {
            "settings-inline-history-enabled"
        } else {
            "settings-inline-history-disabled"
        };

        let message = utils::get_message(&bundle, message_key, None);

        models::UserConfig::set(
            &cx.pool,
            models::UserConfigKey::InlineHistory,
            &callback_query.from,
            enabled,
        )
        .await?;

        if !enabled {
            let mut redis = cx.redis.clone();
            redis
                .del(format!("inline-history:{}", callback_query.from.id))
                .await?;
        }

        let answer = AnswerCallbackQuery {
            callback_query_id: callback_query.id.clone(),
            text: Some(message),
            ..Default::default()
        };

        let keyboard = inline_history_keyboard(cx, &callback_query.from, !enabled).await;

        let edit_message = EditMessageReplyMarkup {
            message_id: Some(reply_message.message_id),
            chat_id: reply_message.chat_id(),
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
            ..Default::default()
        };

        futures::try_join!(cx.make_request(&edit_message), cx.make_request(&answer))?;
    } else {
        let enabled = models::UserConfig::get(
            &cx.pool,
            models::UserConfigKey::InlineHistory,
            &callback_query.from,
        )
        .await?
        .unwrap_or(false);

        let keyboard = inline_history_keyboard(cx, &callback_query.from, !enabled).await;

        let edit_message = EditMessageText {
            message_id: Some(reply_message.message_id),
            chat_id: reply_message.chat_id(),
            text,
            reply_markup: Some(ReplyMarkup::InlineKeyboardMarkup(keyboard)),
            ..Default::default()
        };

        cx.make_request(&edit_message).await?;
    }

    Ok(Completed)
}

async fn send_settings_message(cx: &Context, message: &Message) -> Result<Message, Error> {
    let from = message
        .from
        .as_ref()
        .and_then(|user| user.language_code.as_deref());

    let bundle = cx.get_fluent_bundle(from).await;

    let (site_preference, inline_history) = (
        utils::get_message(&bundle, "settings-site-preference", None),
        utils::get_message(&bundle, "settings-inline-history", None),
    );

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

    let text = utils::get_message(&bundle, "settings-main", None);

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

    let sent_message = cx.make_request(&message).await?;

    Ok(sent_message)
}

async fn sort_order_keyboard<U: Into<User>>(
    conn: &sqlx::Pool<sqlx::Postgres>,
    user: U,
) -> Result<InlineKeyboardMarkup, Error> {
    let user = user.into();

    let row: Option<Vec<String>> =
        models::UserConfig::get(conn, models::UserConfigKey::SiteSortOrder, &user).await?;
    let mut sites = match row {
        Some(row) => row.iter().map(|item| item.parse().unwrap()).collect(),
        None => models::Sites::default_order(),
    };

    let mut buttons = vec![];

    // If the available sites has changed, reset ordering to add new items.
    if sites.len() != models::Sites::len() {
        sites = models::Sites::default_order();
        models::UserConfig::delete(conn, models::UserConfigKey::SiteSortOrder, user).await?;
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
