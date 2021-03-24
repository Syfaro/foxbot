use foxbot_utils::*;
use futures::{stream::StreamExt, TryFutureExt};
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{
    cluster::{Cluster, ShardScheme},
    Event,
};
use twilight_http::Client as HttpClient;
use twilight_model::{
    channel::{Channel, ChannelType, GuildChannel},
    gateway::Intents,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let token = std::env::var("DISCORD_TOKEN").expect("Missing DISCORD_TOKEN");

    let fuzzysearch = std::sync::Arc::new(fuzzysearch::FuzzySearch::new(
        std::env::var("FAUTIL_APITOKEN").expect("Missing FAUTIL_APITOKEN"),
    ));

    let scheme = ShardScheme::Auto;

    let cluster = Cluster::builder(
        &token,
        Intents::GUILDS
            | Intents::GUILD_MESSAGES
            | Intents::GUILD_MESSAGE_REACTIONS
            | Intents::DIRECT_MESSAGES
            | Intents::DIRECT_MESSAGE_REACTIONS,
    )
    .shard_scheme(scheme)
    .build()
    .await
    .expect("Unable to create cluster");

    {
        let cluster = cluster.clone();
        tokio::spawn(async move {
            cluster.up().await;
        });
    }

    let http = HttpClient::new(&token);

    let cache = InMemoryCache::builder()
        .resource_types(ResourceType::all())
        .build();

    let mut events = cluster.events();

    while let Some((shard_id, event)) = events.next().await {
        cache.update(&event);

        let http = http.clone();
        let cache = cache.clone();
        let fuzzysearch = fuzzysearch.clone();
        tokio::spawn(async move {
            if let Err(err) =
                handle_event(shard_id, event.clone(), http.clone(), cache, fuzzysearch).await
            {
                tracing::error!("Error handling event: {:?}", err);

                if let Some((channel_id, _message_id)) = get_event_channel(&event) {
                    if let Err(err) = match err.downcast::<UserErrorMessage>() {
                        Ok(err) => err.create_message(&http, channel_id).await,
                        Err(err) => err.create_message(&http, channel_id).await,
                    } {
                        tracing::error!("Unable to send error message: {:?}", err);
                    }
                }
            }
        });
    }
}

#[tracing::instrument(err, skip(event, http, cache, fuzzysearch))]
async fn handle_event(
    shard_id: u64,
    event: Event,
    http: HttpClient,
    cache: InMemoryCache,
    fuzzysearch: std::sync::Arc<fuzzysearch::FuzzySearch>,
) -> anyhow::Result<()> {
    tracing::trace!("Got event: {:?}", event);

    match &event {
        Event::MessageCreate(msg) => {
            tracing::trace!("Got message: {:?}", msg);

            if msg.author.bot {
                tracing::debug!("Ignoring message from bot");
                return Ok(());
            }

            let is_pm = match cache.guild_channel(msg.channel_id) {
                Some(channel) => {
                    matches!(&*channel, GuildChannel::Text(text) if text.kind == ChannelType::Private)
                }
                _ => {
                    let channel = http
                        .channel(msg.channel_id)
                        .await
                        .map_err(|err| {
                            UserErrorMessage::new(err, "Unable to check channel context")
                        })?
                        .ok_or_else(|| {
                            UserErrorMessage::new(
                                anyhow::format_err!("ChannelId did not exist: {}", msg.channel_id),
                                "Channel did not load",
                            )
                        })?;

                    matches!(channel, Channel::Private(_))
                }
            };

            if !is_pm {
                return Ok(());
            }

            if msg.attachments.is_empty() {
                // Only respond to messages without media in private messages.
                http.create_message(msg.channel_id)
                    .reply(msg.id)
                    .content("No attachments found!")?
                    .await?;

                return Ok(());
            }

            for attachment in &msg.attachments {
                http.create_typing_trigger(msg.channel_id).await?;

                let hash = hash_attachment(&attachment.proxy_url).await?;
                let files = lookup_hash(&fuzzysearch, hash, Some(3)).await?;

                if files.is_empty() {
                    http.create_message(msg.channel_id)
                        .reply(msg.id)
                        .content("No matches found, sorry.")?
                        .await?;

                    continue;
                }

                let urls = files
                    .into_iter()
                    .map(|file| format!("<{}>", file.url()))
                    .collect::<Vec<_>>()
                    .join("\n");

                http.create_message(msg.channel_id)
                    .reply(msg.id)
                    .content(urls)?
                    .await?;
            }
        }
        Event::ReactionAdd(reaction) => {
            use twilight_embed_builder::{EmbedBuilder, EmbedFooterBuilder, ImageSource};
            use twilight_model::channel::ReactionType;

            if !matches!(&reaction.emoji, ReactionType::Unicode { name } if name == "🦊") {
                return Ok(());
            }

            tracing::debug!("Added reaction: {:?}", reaction);

            let (author, attachment) = match cache.message(reaction.channel_id, reaction.message_id)
            {
                Some(message) => (
                    message.author,
                    message
                        .attachments
                        .first()
                        .map(|attachment| attachment.to_owned()),
                ),
                None => {
                    let message = match http
                        .message(reaction.channel_id, reaction.message_id)
                        .await?
                    {
                        Some(message) => message,
                        None => {
                            tracing::warn!("Could not get message from reaction");
                            return Ok(());
                        }
                    };

                    (
                        message.author.id,
                        message
                            .attachments
                            .first()
                            .map(|attachment| attachment.to_owned()),
                    )
                }
            };

            let attachment = match attachment {
                Some(attachment) => attachment,
                None => {
                    tracing::debug!("Message had no attachments");
                    return Ok(());
                }
            };

            let hash = hash_attachment(&attachment.proxy_url).await?;
            let files = lookup_hash(&fuzzysearch, hash, Some(3)).await?;

            if files.is_empty() {
                tracing::debug!("No matches were found");
                return Ok(());
            }

            let urls = files
                .into_iter()
                .map(|file| format!("<{}>", file.url()))
                .collect::<Vec<_>>()
                .join("\n");

            let embed = EmbedBuilder::new()
                .title("Image Sources")?
                .description(urls)?
                .thumbnail(ImageSource::url(attachment.url.to_string())?)
                .footer(EmbedFooterBuilder::new("fuzzysearch.net")?)
                .build()?;

            let private_channel = http.create_private_channel(author).await?;
            http.create_message(private_channel.id)
                .embed(embed)?
                .await?;
        }
        Event::GatewayHeartbeatAck
        | Event::GatewayHello(_)
        | Event::GatewayInvalidateSession(_)
        | Event::GuildCreate(_)
        | Event::MessageUpdate(_)
        | Event::Ready(_)
        | Event::Resumed
        | Event::ShardConnected(_)
        | Event::ShardConnecting(_)
        | Event::ShardDisconnected(_)
        | Event::ShardIdentifying(_)
        | Event::ShardReconnecting(_)
        | Event::ShardResuming(_) => (),
        ev => {
            tracing::warn!("Got had unhandled event: {:?}", ev);
        }
    }

    Ok(())
}

trait CreateErrorMessage<'a> {
    fn create_message(
        &self,
        http: &'a twilight_http::Client,
        channel_id: twilight_model::id::ChannelId,
    ) -> twilight_http::request::channel::message::CreateMessage<'a>;
}

impl<'a> CreateErrorMessage<'a> for anyhow::Error {
    fn create_message(
        &self,
        http: &'a twilight_http::Client,
        channel_id: twilight_model::id::ChannelId,
    ) -> twilight_http::request::channel::message::CreateMessage<'a> {
        http.create_message(channel_id)
            .content("An error occured.")
            .unwrap()
    }
}

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
        &self,
        http: &'a twilight_http::Client,
        channel_id: twilight_model::id::ChannelId,
    ) -> twilight_http::request::channel::message::CreateMessage<'a> {
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

fn get_event_channel(
    event: &Event,
) -> Option<(
    twilight_model::id::ChannelId,
    Option<twilight_model::id::MessageId>,
)> {
    match event {
        Event::MessageCreate(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageDelete(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::MessageUpdate(msg) => Some((msg.channel_id, Some(msg.id))),
        Event::ReactionAdd(re) => Some((re.channel_id, Some(re.message_id))),
        Event::ReactionRemove(re) => Some((re.channel_id, Some(re.message_id))),
        _ => None,
    }
}
