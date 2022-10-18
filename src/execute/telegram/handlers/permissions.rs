use async_trait::async_trait;
use tgbotapi::{ChatMemberUpdated, ChatType};

use crate::{execute::telegram::Context, models, Error};

use super::{
    Handler,
    Status::{self, Completed, Ignored},
};

pub struct PermissionHandler;

#[async_trait]
impl Handler for PermissionHandler {
    fn name(&self) -> &'static str {
        "permissions"
    }

    async fn handle(
        &self,
        cx: &Context,
        update: &tgbotapi::Update,
        _command: Option<&tgbotapi::Command>,
    ) -> Result<Status, Error> {
        if let tgbotapi::Update {
            message:
                Some(tgbotapi::Message {
                    chat: tgbotapi::Chat { id: chat_id, .. },
                    migrate_from_chat_id: Some(from_id),
                    ..
                }),
            ..
        } = update
        {
            migrate_chat(cx, *chat_id, *from_id).await?;
            return Ok(Completed);
        }

        if handle_my_chat_member(cx, &update.my_chat_member).await? {
            handle_chat_member(cx, &update.my_chat_member).await?;
            return Ok(Completed);
        }

        if handle_chat_member(cx, &update.chat_member).await? {
            return Ok(Completed);
        }

        Ok(Ignored)
    }
}

async fn handle_my_chat_member(
    cx: &Context,
    my_chat_member: &Option<ChatMemberUpdated>,
) -> Result<bool, Error> {
    let my_chat_member = match my_chat_member {
        Some(my_chat_member) => my_chat_member,
        _ => return Ok(false),
    };

    tracing::info!("got updated my chat member info: {:?}", my_chat_member);

    let can_delete = my_chat_member
        .new_chat_member
        .can_delete_messages
        .unwrap_or(false);

    if let Err(err) = models::GroupConfig::set(
        &cx.pool,
        models::GroupConfigKey::HasDeletePermission,
        &my_chat_member.chat,
        can_delete,
    )
    .await
    {
        tracing::error!("unable to set group delete permission: {:?}", err);
    }

    if my_chat_member.chat.chat_type == ChatType::Channel {
        if let Err(err) = models::GroupConfig::set(
            &cx.pool,
            models::GroupConfigKey::CanEditChannel,
            &my_chat_member.chat,
            my_chat_member
                .new_chat_member
                .can_edit_messages
                .unwrap_or(false),
        )
        .await
        {
            tracing::error!("unable to set channel edit permission: {:?}", err);
        }
    }

    if let Err(err) = models::Permission::add_change(&cx.pool, my_chat_member).await {
        tracing::error!("unable to save permission change: {:?}", err);
    }

    Ok(true)
}

async fn handle_chat_member(
    cx: &Context,
    chat_member: &Option<ChatMemberUpdated>,
) -> Result<bool, Error> {
    let chat_member = match chat_member {
        Some(chat_member) => chat_member,
        _ => return Ok(false),
    };

    tracing::debug!("got updated chat member info: {:?}", chat_member);

    match models::ChatAdmin::update_chat(
        &cx.pool,
        &chat_member.chat,
        &chat_member.new_chat_member.user,
        &chat_member.new_chat_member.status,
        Some(chat_member.date),
    )
    .await
    {
        Ok(is_admin) => {
            tracing::debug!(
                user_id = chat_member.new_chat_member.user.id,
                is_admin,
                "updated user"
            );

            // Handle loading some data when the bot's administrative status
            // changes.
            if chat_member.new_chat_member.user.id == cx.bot_user.id {
                if let Err(err) = handle_bot_update(cx, chat_member, is_admin).await {
                    tracing::error!("unable to update requested data: {:?}", err);
                }
            }
        }
        Err(err) => {
            tracing::error!("unable to save permission change: {:?}", err);
        }
    }

    Ok(true)
}

async fn handle_bot_update(
    cx: &Context,
    chat_member: &ChatMemberUpdated,
    is_admin: bool,
) -> Result<(), Error> {
    tracing::debug!("bot permissions changed");

    if !is_admin {
        tracing::warn!("bot has lost admin permissions, discarding potentially stale data");

        models::ChatAdmin::flush(&cx.pool, &chat_member.chat, cx.bot_user.id).await?;
    } else {
        let get_chat_administrators = tgbotapi::requests::GetChatAdministrators {
            chat_id: chat_member.chat.id.into(),
        };

        let admins = cx.bot.make_request(&get_chat_administrators).await?;

        for admin in admins {
            tracing::trace!(user_id = admin.user.id, "discovered group admin");

            models::ChatAdmin::update_chat(
                &cx.pool,
                &chat_member.chat,
                &admin.user,
                &admin.status,
                None,
            )
            .await?;
        }
    }

    Ok(())
}

#[tracing::instrument(err, skip(cx))]
async fn migrate_chat(cx: &Context, chat_id: i64, from_id: i64) -> Result<(), Error> {
    tracing::warn!("got chat migration");

    let mut tx = cx.pool.begin().await?;

    sqlx::query!("LOCK TABLE chat, chat_telegram IN EXCLUSIVE MODE")
        .execute(&mut tx)
        .await?;

    let new_chat_exists = !sqlx::query_scalar!(
        "SELECT 1 FROM chat_telegram WHERE telegram_id = $1",
        chat_id
    )
    .fetch_all(&mut tx)
    .await?
    .is_empty();

    let old_chat_exists = !sqlx::query_scalar!(
        "SELECT 1 FROM chat_telegram WHERE telegram_id = $1",
        from_id
    )
    .fetch_all(&mut tx)
    .await?
    .is_empty();

    tracing::debug!(
        new_chat_exists,
        old_chat_exists,
        "checked if chats previously existed"
    );

    // If they've both already been added, we have to make sure
    // everything is pointing at the right data.
    //
    // This isn't ideal, but it saves having to pull everything through another
    // table and this is rarely executed.
    if new_chat_exists && old_chat_exists {
        tracing::debug!("both chats had been used, checking if rewrite is needed");

        // Collect the ID pointed to by each Telegram chat ID
        let wanted_id = sqlx::query_scalar!("SELECT lookup_chat_by_telegram_id($1)", from_id)
            .fetch_one(&mut tx)
            .await?
            .unwrap();
        let other_id = sqlx::query_scalar!("SELECT lookup_chat_by_telegram_id($1)", chat_id)
            .fetch_one(&mut tx)
            .await?
            .unwrap();

        // If they're not the same, delete one and rewrite the data
        if wanted_id != other_id {
            tracing::warn!("both chats have been used, rewriting data");

            // Point Telegram ID to new chat
            sqlx::query!(
                "UPDATE chat_telegram SET chat_id = $1 WHERE telegram_id = $2",
                wanted_id,
                chat_id
            )
            .execute(&mut tx)
            .await?;

            // Update all tables that reference a chat ID to use the new chat ID
            sqlx::query!(
                "UPDATE chat_administrator SET chat_id = $1 WHERE chat_id = $2",
                wanted_id,
                other_id
            )
            .execute(&mut tx)
            .await?;
            sqlx::query!(
                "UPDATE group_config SET chat_id = $1 WHERE chat_id = $2",
                wanted_id,
                other_id
            )
            .execute(&mut tx)
            .await?;
            sqlx::query!(
                "UPDATE permission SET chat_id = $1 WHERE chat_id = $2",
                wanted_id,
                other_id
            )
            .execute(&mut tx)
            .await?;
            sqlx::query!(
                "UPDATE video_job_message SET chat_id = $1 WHERE chat_id = $2",
                wanted_id,
                other_id
            )
            .execute(&mut tx)
            .await?;

            // Remove unused old chat, this will also catch anything that didn't
            // get updated
            sqlx::query!("DELETE FROM chat WHERE id = $1", other_id)
                .execute(&mut tx)
                .await?;
        }
    // Otherwise, we can add the new chat without rewriting anything.
    } else {
        tracing::debug!("adding new telegram id to chat");

        // Determine which ID we already have and need to look up and
        // which ID is going to be associated with the chat.
        let (lookup_id, associate_id) = if new_chat_exists {
            (chat_id, from_id)
        } else {
            (from_id, chat_id)
        };

        let chat_id = sqlx::query_scalar!("SELECT lookup_chat_by_telegram_id($1)", lookup_id)
            .fetch_one(&mut tx)
            .await?
            .unwrap();

        sqlx::query!(
            "INSERT INTO chat_telegram (chat_id, telegram_id) VALUES ($1, $2)",
            chat_id,
            associate_id
        )
        .execute(&mut tx)
        .await?;
    }

    tx.commit().await?;

    Ok(())
}
