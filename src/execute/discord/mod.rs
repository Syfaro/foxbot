use std::sync::Arc;

use futures::{stream::StreamExt, TryFutureExt};
use tokio::sync::Mutex;
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{cluster::Cluster, Event};
use twilight_http::{
    client::InteractionClient, request::channel::message::CreateMessage, Client as HttpClient,
};
use twilight_model::{
    application::interaction::{ApplicationCommand, Interaction},
    channel::{embed::Embed, message::MessageFlags, Attachment, ChannelType, Message},
    gateway::Intents,
    guild::Permissions,
    http::interaction::{InteractionResponse, InteractionResponseType},
    id::{
        marker::{ApplicationMarker, ChannelMarker, GuildMarker, MessageMarker, UserMarker},
        Id,
    },
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::{
    models,
    sites::PostInfo,
    utils::{self, SiteCallback},
    Args, DiscordConfig, Features, RunConfig,
};

pub async fn discord(args: Args, config: RunConfig, discord_config: DiscordConfig) {
    let fuzzysearch = std::sync::Arc::new(fuzzysearch::FuzzySearch::new_with_opts(
        fuzzysearch::FuzzySearchOpts {
            endpoint: config.fuzzysearch_endpoint.clone(),
            api_key: config.fuzzysearch_api_token.clone(),
            client: None,
        },
    ));

    let pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("Unable to connect to database");

    crate::run_migrations(&pool).await;

    let redis = redis::Client::open(config.redis_url.clone()).expect("could not connect to redis");
    let redis = redis::aio::ConnectionManager::new(redis)
        .await
        .expect("could not create redis connection manager");

    let unleash = foxlib::flags::client::<Features>(
        "foxbot-discord",
        &args.unleash_host,
        args.unleash_secret,
    )
    .await
    .expect("unable to register unleash");

    let sites = crate::sites::get_all_sites(&config, pool.clone(), unleash.clone()).await;

    let sites = Arc::new(Mutex::new(sites));

    let cluster = Cluster::builder(
        discord_config.discord_token.clone(),
        Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES,
    );

    let (cluster, mut events) = cluster.build().await.expect("Unable to create cluster");

    cluster.up().await;

    let http = Arc::new(HttpClient::new(discord_config.discord_token.clone()));

    let application_id = http
        .current_user_application()
        .exec()
        .await
        .expect("could not get current user application")
        .model()
        .await
        .expect("could not get model")
        .id;

    let cache = Arc::new(
        InMemoryCache::builder()
            .resource_types(ResourceType::all())
            .build(),
    );

    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));

    while let Some((_shard_id, event)) = events.next().await {
        cache.update(&event);

        let http = http.clone();
        let cache = cache.clone();
        let redis = redis.clone();
        let sites = sites.clone();
        let fuzzysearch = fuzzysearch.clone();
        let semaphore = semaphore.clone();

        let discord_config = discord_config.clone();
        tokio::spawn(async move {
            let _permit = semaphore.acquire().await.expect("Unable to acquire permit");

            let context = Context {
                http: http.clone(),
                application_id,
                cache,
                redis,
                config: discord_config,
                sites,
                fuzzysearch,
            };

            sentry::start_session();

            if let Err(err) = handle_event(event.clone(), context).await {
                tracing::error!("Error handling event: {:?}", err);

                if let Some((channel_id, message_id)) = get_event_channel(&event) {
                    let create_message = err.create_message(&http, channel_id);

                    let create_message = if let Some(message_id) = message_id {
                        create_message.reply(message_id)
                    } else {
                        create_message
                    };

                    if let Err(err) = create_message.exec().await {
                        tracing::error!("Unable to send error message: {:?}", err);
                    }
                }

                sentry::end_session_with_status(sentry::protocol::SessionStatus::Abnormal);
            } else {
                sentry::end_session_with_status(sentry::protocol::SessionStatus::Exited);
            }
        });
    }

    tracing::warn!("Bot has stopped.");
}

struct Context {
    http: Arc<HttpClient>,
    application_id: Id<ApplicationMarker>,
    cache: Arc<InMemoryCache>,
    redis: redis::aio::ConnectionManager,
    config: DiscordConfig,

    sites: Arc<Mutex<Vec<crate::sites::BoxedSite>>>,
    fuzzysearch: std::sync::Arc<fuzzysearch::FuzzySearch>,
}

#[allow(clippy::single_match)]
#[tracing::instrument(err, skip(event, ctx))]
async fn handle_event(event: Event, ctx: Context) -> anyhow::Result<()> {
    tracing::trace!("Got event: {:?}", event);

    match &event {
        Event::MessageCreate(msg) => {
            tracing::trace!("Got message: {:?}", msg);

            if msg.author.bot {
                tracing::debug!("Ignoring message from bot");
                return Ok(());
            }

            if !is_pm(&ctx, msg.channel_id).await? && !is_mention(&ctx, msg).await? {
                return Ok(());
            }

            let (msg_id, attachments, content) =
                if let Some(referenced_msg) = &msg.referenced_message {
                    (
                        referenced_msg.id,
                        &referenced_msg.attachments,
                        &referenced_msg.content,
                    )
                } else {
                    (msg.id, &msg.attachments, &msg.content)
                };

            if !attachments.is_empty() {
                source_attachments(
                    &ctx,
                    msg.guild_id,
                    msg.channel_id,
                    msg.id,
                    msg_id,
                    attachments,
                )
                .await?;
            } else if let Some(links) = extract_links(content) {
                mirror_links(
                    &ctx,
                    msg.guild_id,
                    msg.channel_id,
                    msg.author.id,
                    msg.id,
                    msg_id,
                    &links,
                )
                .await?;
            } else {
                ctx.http
                    .create_message(msg.channel_id)
                    .reply(msg.id)
                    .content("I don't know what to do here, sorry!")?
                    .exec()
                    .await?;
            }
        }
        Event::InteractionCreate(interaction) => match &interaction.0 {
            Interaction::ApplicationCommand(command) => {
                let interaction_client = ctx.http.interaction(ctx.application_id);

                if command.data.id == ctx.config.find_source_command {
                    find_sources_interaction(&ctx, &interaction_client, command).await?;
                } else if command.data.id == ctx.config.share_images_command {
                    share_images_interaction(&ctx, &interaction_client, command).await?;
                } else {
                    tracing::warn!("Got unknown command: {:?}", command);
                }
            }
            _ => (),
        },
        _ => (),
    }

    Ok(())
}

trait CreateErrorMessage<'a> {
    fn create_message(
        &'a self,
        http: &'a HttpClient,
        channel_id: Id<ChannelMarker>,
    ) -> CreateMessage<'a>;
}

impl<'a> CreateErrorMessage<'a> for anyhow::Error {
    fn create_message(
        &self,
        http: &'a HttpClient,
        channel_id: Id<ChannelMarker>,
    ) -> CreateMessage<'a> {
        http.create_message(channel_id)
            .content("An error occured.")
            .unwrap()
    }
}

/// An error message that can be displayed to the user.
#[derive(Debug, thiserror::Error)]
struct UserErrorMessage<'a> {
    pub msg: std::borrow::Cow<'a, str>,
    #[source]
    source: anyhow::Error,
}

impl<'a> UserErrorMessage<'a> {
    fn new<S, M>(source: S, msg: M) -> Self
    where
        S: Into<anyhow::Error>,
        M: Into<std::borrow::Cow<'a, str>>,
    {
        Self {
            msg: msg.into(),
            source: source.into(),
        }
    }
}

impl<'a> CreateErrorMessage<'a> for UserErrorMessage<'_> {
    fn create_message(
        &'a self,
        http: &'a HttpClient,
        channel_id: Id<ChannelMarker>,
    ) -> CreateMessage<'a> {
        http.create_message(channel_id).content(&self.msg).unwrap()
    }
}

impl std::fmt::Display for UserErrorMessage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error: {}", self.msg)
    }
}

async fn find_sources_interaction(
    ctx: &Context,
    interaction_client: &InteractionClient<'_>,
    command: &ApplicationCommand,
) -> anyhow::Result<()> {
    let messages = command
        .data
        .resolved
        .as_ref()
        .map(|resolved| resolved.messages.values().collect::<Vec<_>>())
        .unwrap_or_default();

    tracing::debug!("Got command with {} messages", messages.len());

    let response = InteractionResponse {
        kind: InteractionResponseType::DeferredChannelMessageWithSource,
        data: Some(
            InteractionResponseDataBuilder::new()
                .flags(MessageFlags::EPHEMERAL)
                .content("Loading images...".to_string())
                .build(),
        ),
    };

    interaction_client
        .create_response(command.id, &command.token, &response)
        .exec()
        .await?;

    let mut embeds = Vec::with_capacity(messages.len());

    for message in messages {
        let attachments = &message.attachments;

        if attachments.is_empty() {
            tracing::info!("Attachments was empty");
            continue;
        }

        for attachment in attachments {
            let hash = if let Some(hash) =
                models::FileCache::get(&ctx.redis, &attachment.proxy_url).await?
            {
                hash
            } else {
                let hash = hash_attachment(&attachment.proxy_url).await?;
                models::FileCache::set(&ctx.redis, &attachment.proxy_url, hash).await?;
                hash
            };

            let files = lookup_hash(&ctx.fuzzysearch, hash, Some(3)).await?;

            if files.is_empty() {
                tracing::info!("No images or sources were found");
                continue;
            }

            let embed = sources_embed(attachment, &files)
                .map_err(|err| UserErrorMessage::new(err, "Unable to properly respond"))?;

            tracing::info!("Created embed: {:?}", embed);

            embeds.push(embed);
        }
    }

    if embeds.is_empty() {
        interaction_client
            .update_response(&command.token)
            .content(Some("No sources were found."))?
            .exec()
            .await?;
    } else {
        interaction_client
            .update_response(&command.token)
            .embeds(Some(&embeds))?
            .exec()
            .await?;
    }

    Ok(())
}

async fn share_images_interaction(
    ctx: &Context,
    interaction_client: &InteractionClient<'_>,
    command: &ApplicationCommand,
) -> anyhow::Result<()> {
    let messages = command
        .data
        .resolved
        .as_ref()
        .map(|resolved| resolved.messages.values().collect::<Vec<_>>())
        .unwrap_or_default();

    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);

    let links: Vec<&str> = messages
        .iter()
        .flat_map(|message| finder.links(&message.content).map(|link| link.as_str()))
        .collect();

    if links.is_empty() {
        let response = InteractionResponse {
            kind: InteractionResponseType::ChannelMessageWithSource,
            data: Some(
                InteractionResponseDataBuilder::new()
                    .flags(MessageFlags::EPHEMERAL)
                    .content("No links found".to_string())
                    .build(),
            ),
        };

        interaction_client
            .create_response(command.id, &command.token, &response)
            .exec()
            .await?;

        return Ok(());
    }

    let response = InteractionResponse {
        kind: InteractionResponseType::DeferredChannelMessageWithSource,
        data: Some(
            InteractionResponseDataBuilder::new()
                .content("Loading links...".to_string())
                .build(),
        ),
    };

    interaction_client
        .create_response(command.id, &command.token, &response)
        .exec()
        .await?;

    let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());

    let _missing = get_images(
        ctx,
        command.user.as_ref().map(|user| user.id),
        &links,
        &mut |info| {
            results.extend(info.results);
        },
    )
    .await?;

    if results.is_empty() {
        interaction_client
            .update_response(&command.token)
            .content(Some("No images found"))?
            .exec()
            .await?;

        return Ok(());
    }

    let embeds: Vec<_> = results
        .into_iter()
        .map(|result| link_embed(&result))
        .collect::<anyhow::Result<_>>()?;

    interaction_client
        .update_response(&command.token)
        .content(None)?
        .embeds(Some(&embeds))?
        .exec()
        .await?;

    Ok(())
}

/// Download an attachment up to 50MB, attempt to load the image, and hash the
/// contents into something suitable for FuzzySearch.
async fn hash_attachment<'a>(proxy_url: &str) -> Result<i64, UserErrorMessage<'a>> {
    let check_bytes = utils::CheckFileSize::new(proxy_url, 50_000_000);
    let bytes = check_bytes
        .into_bytes()
        .await
        .map_err(|err| UserErrorMessage::new(err, "Unable to download image"))?;

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&bytes))
        .await
        .map_err(|err| UserErrorMessage::new(err, "Internal error"))?
        .map_err(|err| UserErrorMessage::new(err, "Unable to load image"))?;

    Ok(hash)
}

/// Lookup a hash on FuzzySearch with an optional distance.
fn lookup_hash(
    fuzzysearch: &std::sync::Arc<fuzzysearch::FuzzySearch>,
    hash: i64,
    distance: Option<i64>,
) -> impl std::future::Future<Output = Result<Vec<fuzzysearch::File>, UserErrorMessage<'static>>> + '_
{
    utils::lookup_single_hash(fuzzysearch, hash, distance)
        .map_err(|err| UserErrorMessage::new(err, "Unable to look up sources"))
}

/// Check the channel and possibily message ID from an event where it would make
/// sense to reply to the message.
fn get_event_channel(event: &Event) -> Option<(Id<ChannelMarker>, Option<Id<MessageMarker>>)> {
    match event {
        Event::MessageCreate(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageDelete(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageUpdate(msg) => Some((msg.channel_id, Some(msg.id))),
        _ => None,
    }
}

/// Check if a channel is a private message, using the cache if possible.
async fn is_pm(
    ctx: &Context,
    channel_id: Id<ChannelMarker>,
) -> Result<bool, UserErrorMessage<'static>> {
    if let Some(channel) = ctx.cache.channel(channel_id) {
        return Ok(channel.kind == ChannelType::Private);
    }

    let channel = ctx
        .http
        .channel(channel_id)
        .exec()
        .await
        .map_err(|err| UserErrorMessage::new(err, "Unable to check channel context"))?
        .model()
        .await
        .map_err(|err| UserErrorMessage::new(err, "Channel did not load"))?;

    Ok(channel.kind == ChannelType::Private)
}

/// Check if a message mentions the bot.
async fn is_mention(ctx: &Context, message: &Message) -> Result<bool, UserErrorMessage<'static>> {
    let current_user_id = match ctx.cache.current_user() {
        Some(user) => user.id,
        None => {
            tracing::warn!("Current user is not cached");

            let current_user = ctx
                .http
                .current_user()
                .exec()
                .await
                .map_err(|err| UserErrorMessage::new(err, "Unable to load bot metadata"))?
                .model()
                .map_err(|err| UserErrorMessage::new(err, "Unable to get current user ID"))
                .await?;

            current_user.id
        }
    };

    let is_mention = message
        .mentions
        .iter()
        .any(|mention| mention.id == current_user_id);

    Ok(is_mention)
}

/// Build an embed for a collection of sources from an attachment.
fn sources_embed(attachment: &Attachment, files: &[fuzzysearch::File]) -> anyhow::Result<Embed> {
    use twilight_util::builder::embed::{EmbedBuilder, EmbedFooterBuilder, ImageSource};

    let urls = files
        .iter()
        .map(|file| format!("<{}>", file.url()))
        .collect::<Vec<_>>()
        .join("\n");

    let embed = EmbedBuilder::new()
        .title("Image Sources")
        .description(urls)
        .thumbnail(ImageSource::url(attachment.url.to_string())?)
        .footer(EmbedFooterBuilder::new("fuzzysearch.net"))
        .build();

    Ok(embed)
}

fn link_embed(post: &PostInfo) -> anyhow::Result<Embed> {
    use twilight_util::builder::embed::{EmbedBuilder, ImageSource};

    let embed = EmbedBuilder::new()
        .title(post.site_name.clone())
        .url(post.source_link.as_deref().unwrap_or(&post.url))
        .image(ImageSource::url(&post.url)?)
        .build();

    Ok(embed)
}

async fn allow_nsfw(ctx: &Context, channel_id: Id<ChannelMarker>) -> Option<bool> {
    if is_pm(ctx, channel_id).await.ok()? {
        return Some(true);
    }

    ctx.cache.channel(channel_id)?.nsfw
}

async fn source_attachments(
    ctx: &Context,
    guild_id: Option<Id<GuildMarker>>,
    channel_id: Id<ChannelMarker>,
    summoning_id: Id<MessageMarker>,
    content_id: Id<MessageMarker>,
    attachments: &[Attachment],
) -> anyhow::Result<()> {
    let allow_nsfw = allow_nsfw(ctx, channel_id).await.unwrap_or(false);
    let mut skip_delete = false;

    for attachment in attachments {
        ctx.http.create_typing_trigger(channel_id).exec().await?;

        let hash =
            if let Some(hash) = models::FileCache::get(&ctx.redis, &attachment.proxy_url).await? {
                hash
            } else {
                let hash = hash_attachment(&attachment.proxy_url).await?;
                models::FileCache::set(&ctx.redis, &attachment.proxy_url, hash).await?;
                hash
            };

        let files = lookup_hash(&ctx.fuzzysearch, hash, Some(3)).await?;

        if files.is_empty() {
            ctx.http
                .create_message(channel_id)
                .reply(summoning_id)
                .content("No matches found, sorry.")?
                .exec()
                .await?;

            skip_delete = true;
            continue;
        }

        if !allow_nsfw
            && files
                .iter()
                .any(|file| !matches!(file.rating, Some(fuzzysearch::Rating::General)))
        {
            ctx.http
                .create_message(channel_id)
                .reply(summoning_id)
                .content("I'll only show NSFW results in NSFW channels.")?
                .exec()
                .await?;

            skip_delete = true;
            continue;
        }

        let embed = sources_embed(attachment, &files)
            .map_err(|err| UserErrorMessage::new(err, "Unable to properly respond"))?;

        ctx.http
            .create_message(channel_id)
            .reply(content_id)
            .embeds(&[embed])?
            .exec()
            .await?;
    }

    if !skip_delete
        && matches!(guild_id, Some(guild_id) if summoning_id != content_id && can_manage_messages(ctx, guild_id).unwrap_or(false))
    {
        ctx.http
            .delete_message(channel_id, summoning_id)
            .exec()
            .await?;
    }

    Ok(())
}

/// Collect links from text.
fn extract_links(content: &str) -> Option<Vec<&str>> {
    let finder = linkify::LinkFinder::new();

    let links: Vec<_> = finder.links(content).map(|link| link.as_str()).collect();

    if links.is_empty() {
        None
    } else {
        Some(links)
    }
}

/// Collect all images from many links.
async fn get_images<'a, C>(
    ctx: &Context,
    user_id: Option<Id<UserMarker>>,
    links: &'a [&str],
    callback: &mut C,
) -> anyhow::Result<Vec<&'a str>>
where
    C: FnMut(SiteCallback),
{
    let mut missing = vec![];

    let mut sites = ctx.sites.lock().await;

    'link: for link in links {
        for site in sites.iter_mut() {
            if site.url_supported(link).await {
                let start = std::time::Instant::now();

                let user = user_id.map(crate::models::User::from);
                let images = site.get_images(user.as_ref(), link).await?;

                match images {
                    Some(results) => {
                        callback(SiteCallback {
                            site,
                            link,
                            duration: start.elapsed().as_millis() as i64,
                            results,
                        });
                    }
                    _ => {
                        missing.push(*link);
                    }
                }

                continue 'link;
            }
        }
    }

    Ok(missing)
}

fn can_manage_messages(ctx: &Context, guild_id: Id<GuildMarker>) -> Option<bool> {
    let current_user = ctx.cache.current_user()?;
    let member = ctx.cache.member(guild_id, current_user.id)?;

    for role_id in member.roles() {
        let role = ctx.cache.role(*role_id)?;

        if role.permissions.contains(Permissions::MANAGE_MESSAGES) {
            return Some(true);
        }
    }

    Some(false)
}

/// Send an embed as a reply for each link contained in a message.
async fn mirror_links(
    ctx: &Context,
    guild_id: Option<Id<GuildMarker>>,
    channel_id: Id<ChannelMarker>,
    user_id: Id<UserMarker>,
    summoning_id: Id<MessageMarker>,
    content_id: Id<MessageMarker>,
    links: &[&str],
) -> anyhow::Result<()> {
    let mut results: Vec<PostInfo> = Vec::with_capacity(links.len());

    let _missing = get_images(ctx, Some(user_id), links, &mut |info| {
        results.extend(info.results);
    })
    .await?;

    for result in results {
        ctx.http
            .create_message(channel_id)
            .reply(content_id)
            .embeds(&[link_embed(&result)?])?
            .exec()
            .await?;
    }

    if matches!(guild_id, Some(guild_id) if summoning_id != content_id && can_manage_messages(ctx, guild_id).unwrap_or(false))
    {
        ctx.http
            .delete_message(channel_id, summoning_id)
            .exec()
            .await?;
    }

    Ok(())
}
