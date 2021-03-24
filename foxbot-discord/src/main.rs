use foxbot_utils::*;
use futures::stream::StreamExt;
use twilight_cache_inmemory::{InMemoryCache, ResourceType};
use twilight_gateway::{
    cluster::{Cluster, ShardScheme},
    Event,
};
use twilight_http::Client as HttpClient;
use twilight_model::gateway::Intents;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let token = std::env::var("DISCORD_TOKEN").expect("Missing DISCORD_TOKEN");

    let fuzzysearch = std::sync::Arc::new(fuzzysearch::FuzzySearch::new(
        std::env::var("FAUTIL_APITOKEN").expect("Missing FAUTIL_APITOKEN"),
    ));

    let scheme = ShardScheme::Auto;

    let cluster = Cluster::builder(&token, Intents::DIRECT_MESSAGES)
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
        .resource_types(ResourceType::MESSAGE)
        .build();

    let mut events = cluster.events();

    while let Some((shard_id, event)) = events.next().await {
        cache.update(&event);

        let http = http.clone();
        let fuzzysearch = fuzzysearch.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_event(shard_id, event, http, fuzzysearch).await {
                tracing::error!("Error handling event: {:?}", err);
            }
        });
    }
}

#[tracing::instrument(skip(event, http, fuzzysearch), err)]
async fn handle_event(
    shard_id: u64,
    event: Event,
    http: HttpClient,
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

            if msg.attachments.is_empty() {
                http.create_message(msg.channel_id)
                    .reply(msg.id)
                    .content("No attachments found!")?
                    .await?;
                return Ok(());
            }

            for attachment in &msg.attachments {
                http.create_typing_trigger(msg.channel_id).await?;

                let check_bytes = CheckFileSize::new(&attachment.proxy_url, 50_000_000);
                let bytes = match check_bytes.into_bytes().await {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        tracing::error!("Unable to download attachment: {:?}", err);
                        http.create_message(msg.channel_id)
                            .reply(msg.id)
                            .content("Unable to download image")?
                            .await?;
                        return Ok(());
                    }
                };

                let hash = match tokio::task::spawn_blocking(move || {
                    fuzzysearch::hash_bytes(&bytes)
                })
                .await
                {
                    Err(err) => {
                        tracing::error!("Unable to spawn image hashing: {:?}", err);
                        http.create_message(msg.channel_id)
                            .reply(msg.id)
                            .content("Unable to process image")?
                            .await?;
                        return Ok(());
                    }
                    Ok(Err(err)) => {
                        tracing::error!("Unable to process image: {:?}", err);
                        http.create_message(msg.channel_id)
                            .reply(msg.id)
                            .content("Unable to process image")?
                            .await?;
                        return Ok(());
                    }
                    Ok(Ok(hash)) => hash,
                };

                let files = match lookup_single_hash(&fuzzysearch, hash, Some(3)).await {
                    Ok(files) => files,
                    Err(err) => {
                        tracing::error!("Unable to lookup hash: {:?}", err);
                        http.create_message(msg.channel_id)
                            .reply(msg.id)
                            .content("Unable to lookup matches")?
                            .await?;
                        return Ok(());
                    }
                };

                if files.is_empty() {
                    http.create_message(msg.channel_id)
                        .reply(msg.id)
                        .content("No matches found, sorry.")?
                        .await?;
                    return Ok(());
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
        Event::GatewayHeartbeatAck
        | Event::ShardIdentifying(_)
        | Event::GatewayHello(_)
        | Event::ShardConnected(_)
        | Event::Ready(_) => (),
        ev => {
            tracing::warn!("Got had unhandled event: {:?}", ev);
        }
    }

    Ok(())
}
