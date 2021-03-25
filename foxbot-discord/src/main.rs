use futures::{stream::StreamExt, TryFutureExt};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{
    cluster::{Cluster, ShardScheme},
    Event,
};
use twilight_http::{request::channel::message::CreateMessage, Client as HttpClient};
use twilight_model::{
    channel::{
        embed::Embed, Attachment, Channel, ChannelType, GuildChannel, Message, ReactionType,
    },
    gateway::Intents,
    guild::Permissions,
    id::{ChannelId, GuildId, MessageId, UserId},
};

use foxbot_models::FileURLCache;
use foxbot_utils::*;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    load_env();
    let config = match envy::from_env::<Config>() {
        Ok(config) => config,
        Err(err) => panic!("{:#?}", err),
    };

    let fuzzysearch = std::sync::Arc::new(fuzzysearch::FuzzySearch::new(
        config.fautil_apitoken.clone(),
    ));

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .expect("Unable to connect to database");

    let sites = foxbot_sites::get_all_sites(
        config.fa_a,
        config.fa_b,
        config.fautil_apitoken,
        config.weasyl_apitoken,
        config.twitter_consumer_key,
        config.twitter_consumer_secret,
        config.inkbunny_username,
        config.inkbunny_password,
        config.e621_login,
        config.e621_api_key,
        pool.clone(),
    )
    .await;

    let sites = Arc::new(Mutex::new(sites));

    let scheme = ShardScheme::Auto;

    let cluster = Cluster::builder(
        &config.discord_token,
        Intents::GUILDS
            | Intents::GUILD_MESSAGES
            | Intents::GUILD_MESSAGE_REACTIONS
            | Intents::DIRECT_MESSAGES
            | Intents::DIRECT_MESSAGE_REACTIONS,
    )
    .shard_scheme(scheme);

    let cluster = if let Some(sessions) = load_sessions(&config.resume_path).await {
        cluster.resume_sessions(sessions)
    } else {
        cluster
    };

    let temp_admin: Option<UserId> = config.temp_admin.map(|admin| admin.into());

    let cluster = cluster.build().await.expect("Unable to create cluster");

    {
        let cluster = cluster.clone();
        tokio::spawn(async move {
            cluster.up().await;
        });
    }

    let http = HttpClient::new(&config.discord_token);

    let cache = InMemoryCache::builder()
        .resource_types(ResourceType::all())
        .build();

    let mut events = cluster.events();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));

    while let Some((_shard_id, event)) = events.next().await {
        cache.update(&event);

        match (temp_admin, &event) {
            (Some(temp_admin), Event::MessageCreate(msg))
                if msg.author.id == temp_admin && msg.content == "/foxbot-stop" =>
            {
                tracing::warn!("Got command to stop bot");

                let sessions = cluster.down_resumable();
                save_sessions(&config.resume_path, sessions)
                    .await
                    .expect("Unable to save sessions");

                break;
            }
            _ => (),
        }

        let http = http.clone();
        let cache = cache.clone();
        let pool = pool.clone();
        let sites = sites.clone();
        let fuzzysearch = fuzzysearch.clone();
        let semaphore = semaphore.clone();

        tokio::spawn(async move {
            let _permit = semaphore.acquire().await.expect("Unable to acquire permit");

            let context = Context {
                http: http.clone(),
                cache,
                pool,
                sites,
                fuzzysearch,
            };

            if let Err(err) = handle_event(event.clone(), context).await {
                tracing::error!("Error handling event: {:?}", err);

                if let Some((channel_id, message_id)) = get_event_channel(&event) {
                    let create_message = match err.downcast::<UserErrorMessage>() {
                        Ok(err) => err.create_message(&http, channel_id),
                        Err(err) => err.create_message(&http, channel_id),
                    };

                    let create_message = if let Some(message_id) = message_id {
                        create_message.reply(message_id)
                    } else {
                        create_message
                    };

                    if let Err(err) = create_message.await {
                        tracing::error!("Unable to send error message: {:?}", err);
                    }
                }
            }
        });
    }

    tracing::warn!("Bot has stopped.");
}

#[cfg(feature = "env")]
fn load_env() {
    dotenv::dotenv().unwrap();
}

#[cfg(not(feature = "env"))]
fn load_env() {}

#[derive(serde::Deserialize, Debug, Clone)]
struct Config {
    // Site config
    fa_a: String,
    fa_b: String,
    weasyl_apitoken: String,
    inkbunny_username: String,
    inkbunny_password: String,
    e621_login: String,
    e621_api_key: String,
    twitter_consumer_key: String,
    twitter_consumer_secret: String,

    fautil_apitoken: String,

    // Bot config
    discord_token: String,
    temp_admin: Option<u64>,
    resume_path: Option<String>,
    database_url: String,
}

type Sessions = std::collections::HashMap<u64, twilight_gateway::shard::ResumeSession>;

async fn save_sessions(path: &Option<String>, sessions: Sessions) -> anyhow::Result<()> {
    let path = match path {
        Some(path) => path,
        None => return Ok(()),
    };

    let contents = serde_json::to_string(&sessions)?;
    let mut f = tokio::fs::File::create(path).await?;
    f.write_all(&contents.as_bytes()).await?;
    Ok(())
}

async fn load_sessions(path: &Option<String>) -> Option<Sessions> {
    let path = match path {
        Some(path) => path,
        None => return None,
    };

    let mut f = tokio::fs::File::open(path).await.ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).await.ok()?;
    serde_json::from_str(&buf).ok()?
}

type Pool = sqlx::Pool<sqlx::Postgres>;

struct Context {
    http: HttpClient,
    cache: InMemoryCache,
    pool: Pool,

    sites: Arc<Mutex<Vec<foxbot_sites::BoxedSite>>>,
    fuzzysearch: std::sync::Arc<fuzzysearch::FuzzySearch>,
}

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

            if !is_pm(&ctx, msg.channel_id).await? && !is_mention(&ctx, &msg).await? {
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
                mirror_links(&ctx, msg.guild_id, msg.channel_id, msg.id, msg_id, &links).await?;
            } else {
                ctx.http
                    .create_message(msg.channel_id)
                    .reply(msg.id)
                    .content("I don't know what to do here, sorry!")?
                    .await?;
            }
        }
        Event::ReactionAdd(reaction) => {
            if !matches!(&reaction.emoji, ReactionType::Unicode { name } if name == "🦊") {
                return Ok(());
            }

            tracing::debug!("Added reaction: {:?}", reaction);

            let attachment = match get_cached_attachment(
                &ctx,
                reaction.channel_id,
                reaction.message_id,
            )
            .await?
            {
                Some(attachment) => attachment,
                None => {
                    tracing::debug!("Message had no attachments");
                    return Ok(());
                }
            };

            let hash = hash_attachment(&attachment.proxy_url).await?;
            let files = lookup_hash(&ctx.fuzzysearch, hash, Some(3)).await?;

            if files.is_empty() {
                tracing::debug!("No matches were found");
                return Ok(());
            }

            let embed = sources_embed(&attachment, &files)
                .map_err(|err| UserErrorMessage::new(err, "Unable to properly respond"))?;

            let private_channel = ctx.http.create_private_channel(reaction.user_id).await?;

            ctx.http
                .create_message(private_channel.id)
                .embed(embed)?
                .await?;
        }
        _ => (),
    }

    Ok(())
}

trait CreateErrorMessage<'a> {
    fn create_message(&self, http: &'a HttpClient, channel_id: ChannelId) -> CreateMessage<'a>;
}

impl<'a> CreateErrorMessage<'a> for anyhow::Error {
    fn create_message(&self, http: &'a HttpClient, channel_id: ChannelId) -> CreateMessage<'a> {
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
    fn create_message(&self, http: &'a HttpClient, channel_id: ChannelId) -> CreateMessage<'a> {
        http.create_message(channel_id)
            .content(self.msg.to_string())
            .unwrap()
    }
}

impl std::fmt::Display for UserErrorMessage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error: {}", self.msg)
    }
}

/// Download an attachment up to 50MB, attempt to load the image, and hash the
/// contents into something suitable for FuzzySearch.
async fn hash_attachment<'a>(proxy_url: &str) -> Result<i64, UserErrorMessage<'a>> {
    let check_bytes = CheckFileSize::new(&proxy_url, 50_000_000);
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
    lookup_single_hash(&fuzzysearch, hash, distance)
        .map_err(|err| UserErrorMessage::new(err, "Unable to look up sources"))
}

/// Check the channel and possibily message ID from an event where it would make
/// sense to reply to the message.
fn get_event_channel(event: &Event) -> Option<(ChannelId, Option<MessageId>)> {
    match event {
        Event::MessageCreate(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageDelete(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageUpdate(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::ReactionAdd(re) => Some((re.channel_id, Some(re.message_id))),
        Event::ReactionRemove(re) => Some((re.channel_id, Some(re.message_id))),
        _ => None,
    }
}

/// Check if a channel is a private message, using the cache if possible.
async fn is_pm(ctx: &Context, channel_id: ChannelId) -> Result<bool, UserErrorMessage<'static>> {
    if let Some(channel) = ctx.cache.guild_channel(channel_id) {
        return Ok(
            matches!(&*channel, GuildChannel::Text(text) if text.kind == ChannelType::Private),
        );
    }

    let channel = ctx
        .http
        .channel(channel_id)
        .await
        .map_err(|err| UserErrorMessage::new(err, "Unable to check channel context"))?
        .ok_or_else(|| {
            UserErrorMessage::new(
                anyhow::format_err!("ChannelId did not exist: {}", channel_id),
                "Channel did not load",
            )
        })?;

    Ok(matches!(channel, Channel::Private(_)))
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
                .await
                .map_err(|err| UserErrorMessage::new(err, "Unable to load bot metadata"))?;

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
    use twilight_embed_builder::{EmbedBuilder, EmbedFooterBuilder, ImageSource};

    let urls = files
        .iter()
        .map(|file| format!("<{}>", file.url()))
        .collect::<Vec<_>>()
        .join("\n");

    let embed = EmbedBuilder::new()
        .title("Image Sources")?
        .description(urls)?
        .thumbnail(ImageSource::url(attachment.url.to_string())?)
        .footer(EmbedFooterBuilder::new("fuzzysearch.net")?)
        .build()?;

    Ok(embed)
}

fn link_embed(post: &foxbot_sites::PostInfo) -> anyhow::Result<Embed> {
    use twilight_embed_builder::{EmbedBuilder, ImageSource};

    let embed = EmbedBuilder::new()
        .title(post.site_name)?
        .url(post.source_link.as_deref().unwrap_or(&post.url))
        .image(ImageSource::url(&post.url)?)
        .build()?;

    Ok(embed)
}

/// Get the attachment from a given channel and message ID, loading from the
/// cache if possible.
async fn get_cached_attachment(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
) -> Result<Option<Attachment>, UserErrorMessage<'static>> {
    if let Some(message) = ctx.cache.message(channel_id, message_id) {
        let attachment = message
            .attachments
            .first()
            .map(|attachment| attachment.to_owned());

        return Ok(attachment);
    }

    let message = match ctx
        .http
        .message(channel_id, message_id)
        .await
        .map_err(|err| UserErrorMessage::new(err, "Unable to get message"))?
    {
        Some(message) => message,
        None => {
            tracing::warn!("Could not get message");
            return Ok(None);
        }
    };

    let attachment = message
        .attachments
        .first()
        .map(|attachment| attachment.to_owned());

    Ok(attachment)
}

async fn allow_nsfw(ctx: &Context, channel_id: ChannelId) -> Option<bool> {
    if is_pm(&ctx, channel_id).await.ok()? {
        return Some(true);
    }

    let channel = ctx.cache.guild_channel(channel_id)?;
    Some(matches!(&*channel, GuildChannel::Text(channel) if channel.nsfw))
}

async fn source_attachments(
    ctx: &Context,
    guild_id: Option<GuildId>,
    channel_id: ChannelId,
    summoning_id: MessageId,
    content_id: MessageId,
    attachments: &[Attachment],
) -> anyhow::Result<()> {
    let allow_nsfw = allow_nsfw(&ctx, channel_id).await.unwrap_or(false);
    let mut skip_delete = false;

    for attachment in attachments {
        ctx.http.create_typing_trigger(channel_id).await?;

        let hash = if let Some(hash) = FileURLCache::get(&ctx.pool, &attachment.proxy_url).await? {
            hash
        } else {
            let hash = hash_attachment(&attachment.proxy_url).await?;
            FileURLCache::set(&ctx.pool, &attachment.proxy_url, hash).await?;
            hash
        };

        let files = lookup_hash(&ctx.fuzzysearch, hash, Some(3)).await?;

        if files.is_empty() {
            ctx.http
                .create_message(channel_id)
                .reply(summoning_id)
                .content("No matches found, sorry.")?
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
                .await?;

            skip_delete = true;
            continue;
        }

        let embed = sources_embed(&attachment, &files)
            .map_err(|err| UserErrorMessage::new(err, "Unable to properly respond"))?;

        ctx.http
            .create_message(channel_id)
            .reply(content_id)
            .embed(embed)?
            .await?;
    }

    if !skip_delete
        && matches!(guild_id, Some(guild_id) if summoning_id != content_id && can_manage_messages(&ctx, guild_id).unwrap_or(false))
    {
        ctx.http.delete_message(channel_id, summoning_id).await?;
    }

    Ok(())
}

/// Collect links from text.
fn extract_links(content: &str) -> Option<Vec<&str>> {
    let finder = linkify::LinkFinder::new();

    let links: Vec<_> = finder.links(&content).map(|link| link.as_str()).collect();

    if links.is_empty() {
        None
    } else {
        Some(links)
    }
}

/// Collect all images from many links.
async fn get_images<'a, C>(
    ctx: &Context,
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

                let images = site.get_images(0, link).await?;

                match images {
                    Some(results) => {
                        callback(SiteCallback {
                            site: &site,
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

fn can_manage_messages(ctx: &Context, guild_id: GuildId) -> Option<bool> {
    let current_user = ctx.cache.current_user()?;
    let member = ctx.cache.member(guild_id, current_user.id)?;

    for role_id in &member.roles {
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
    guild_id: Option<GuildId>,
    channel_id: ChannelId,
    summoning_id: MessageId,
    content_id: MessageId,
    links: &[&str],
) -> anyhow::Result<()> {
    let mut results: Vec<foxbot_sites::PostInfo> = Vec::with_capacity(links.len());

    let _missing = get_images(&ctx, links, &mut |info| {
        results.extend(info.results);
    })
    .await?;

    for result in results {
        ctx.http
            .create_message(channel_id)
            .reply(content_id)
            .embed(link_embed(&result)?)?
            .await?;
    }

    if matches!(guild_id, Some(guild_id) if summoning_id != content_id && can_manage_messages(&ctx, guild_id).unwrap_or(false))
    {
        ctx.http.delete_message(channel_id, summoning_id).await?;
    }

    Ok(())
}
