use std::sync::Arc;

use async_recursion::async_recursion;
use fuzzysearch::FuzzySearch;
use serde::{Deserialize, Serialize};

use crate::{
    services::faktory::{BotJob, FaktoryClient, FaktoryWorkerEnvironment, JobQueue},
    Error, RedditConfig, RunConfig,
};

const USER_AGENT: &str = concat!(
    "FoxBot for Reddit v",
    env!("CARGO_PKG_VERSION"),
    " developed by @Syfaro"
);

struct Context {
    faktory: FaktoryClient,
    redlock: redlock::RedLock,
    pool: sqlx::PgPool,

    fuzzysearch: FuzzySearch,

    reddit: roux::Me,
    reddit_client: reqwest::Client,
}

pub async fn reddit(config: RunConfig, reddit_config: RedditConfig) {
    let faktory = FaktoryClient::connect(&config.faktory_url)
        .await
        .expect("could not connect to faktory");

    let me = roux::Reddit::new(
        USER_AGENT,
        &reddit_config.reddit_client_id,
        &reddit_config.reddit_client_secret,
    )
    .username(&reddit_config.reddit_username)
    .password(&reddit_config.reddit_password)
    .login()
    .await
    .expect("could not sign into reddit");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", me.access_token)).unwrap(),
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(USER_AGENT),
    );

    let reddit_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .default_headers(headers)
            .build()
            .expect("Unable to create client");

    let faktory_clone = faktory.clone();
    tokio::task::spawn(async move {
        let job = LoadMentionsJob;

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));

        loop {
            interval.tick().await;

            tracing::debug!("enqueueing job to check mentions");
            faktory_clone.enqueue_job(job.clone(), None).await.unwrap();
        }
    });

    let fuzzysearch = fuzzysearch::FuzzySearch::new_with_opts(fuzzysearch::FuzzySearchOpts {
        endpoint: config.fuzzysearch_endpoint,
        api_key: config.fuzzysearch_api_token,
        client: None,
    });
    let redlock = redlock::RedLock::new(vec![config.redis_url]);

    let pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("could not connect to database");
    crate::run_migrations(&pool).await;

    let cx = Arc::new(Context {
        faktory,
        redlock,
        pool,

        fuzzysearch,

        reddit: me,
        reddit_client,
    });

    let mut faktory_environment: FaktoryWorkerEnvironment<_, Error> =
        FaktoryWorkerEnvironment::new(cx);

    faktory_environment.register::<LoadMentionsJob, _, _, _>(|cx, _job, _args| async move {
        tracing::info!("loading mentions");

        loop {
            let messages = cx.reddit.unread().await?;
            if messages.data.children.is_empty() {
                tracing::debug!("no unread messages found");
                break;
            }

            tracing::info!("found {} unread messages", messages.data.children.len());
            let mut handled_names = Vec::with_capacity(messages.data.children.len());

            let jobs = messages
                .data
                .children
                .into_iter()
                .map(|message| ProcessMentionJob {
                    name: message.data.name,
                    author: message.data.author.unwrap_or_else(|| "unknown".to_string()),
                    parent_id: message.data.parent_id,
                });

            for job in jobs {
                let name = job.name.clone();

                match cx.faktory.enqueue_job(job, None).await {
                    Ok(_) => handled_names.push(name),
                    Err(err) => {
                        tracing::error!("could not enqueue job: {}", err);
                    }
                }
            }

            tracing::info!("enqueued {} messages", handled_names.len());

            let ids = handled_names.join(",");
            cx.reddit.mark_read(&ids).await?;

            if messages.data.after.is_none() {
                tracing::debug!("no more messages");
                break;
            }
        }

        Ok(())
    });

    faktory_environment.register::<ProcessMentionJob, _, _, _>(
        |cx, _job, mention_data| async move {
            let parent_id = match mention_data.parent_id {
                Some(parent_id) => parent_id,
                None => {
                    tracing::warn!("mention did not have parent id");
                    return Ok(());
                }
            };

            let lock_key = format!("reddit-mention:{}", mention_data.name);
            let lock = loop {
                if let Some(lock) = cx.redlock.lock(lock_key.as_bytes(), 30 * 1000).await {
                    break lock;
                }
            };

            if sqlx::query_file_scalar!(
                "queries/reddit_mention/check_processed.sql",
                mention_data.name
            )
            .fetch_one(&cx.pool)
            .await?
            .unwrap_or(false)
            {
                tracing::info!("mention was already in database, skipping");
                cx.redlock.unlock(&lock).await;
                return Ok(());
            }

            let (mut reply, hash) = generate_reply(
                &cx.reddit_client,
                &cx.fuzzysearch,
                &parent_id,
                &mention_data.author,
            )
            .await?;

            reply.push_str(
                "\n\n_This bot got these results from [FuzzySearch](https://fuzzysearch.net)._",
            );

            let comment_id = match post_reply(&cx.reddit_client, &mention_data.name, reply).await {
                Some(comment_id) => comment_id,
                None => {
                    tracing::warn!("reddit comment was not posted");
                    return Ok(());
                }
            };

            sqlx::query_file!(
                "queries/reddit_mention/insert_processed.sql",
                mention_data.name,
                hash,
                comment_id,
            )
            .execute(&cx.pool)
            .await?;

            tracing::info!("posted comment");
            cx.redlock.unlock(&lock).await;

            Ok(())
        },
    );

    let mut environment = faktory_environment.finalize();
    environment.labels(vec!["foxbot-reddit".to_string()]);

    let faktory = environment.connect(Some(&config.faktory_url)).unwrap();

    faktory.run_to_completion(&RedditJobQueue::priority_order());
}

#[async_recursion]
async fn find_link(
    reddit_client: &reqwest::Client,
    parent_id: &str,
) -> Result<Option<String>, Error> {
    let url = match reddit_info_one(reddit_client, parent_id).await? {
        RedditInfo::Comment {
            link_id: Some(link_id),
            ..
        } => match reddit_info_one(reddit_client, &link_id).await? {
            RedditInfo::Comment { .. } => {
                tracing::warn!("link_id returned comment, skipping");
                None
            }
            RedditInfo::Link { url, .. } => Some(url),
        },
        RedditInfo::Link {
            crosspost_parent: Some(crosspost_parent),
            ..
        } => {
            tracing::debug!("was crosspost, fetching parent");
            return find_link(reddit_client, &crosspost_parent).await;
        }
        RedditInfo::Link { url, .. } => Some(url),
        _ => {
            tracing::warn!("missing reddit data to process mention");
            None
        }
    };

    Ok(url)
}

async fn generate_reply(
    reddit_client: &reqwest::Client,
    fuzzysearch: &fuzzysearch::FuzzySearch,
    parent_id: &str,
    author: &str,
) -> Result<(String, Option<i64>), Error> {
    let url = match find_link(reddit_client, parent_id).await? {
        Some(url) => url,
        None => {
            return Ok((
                format!(
                    "Hey {}, I couldn't find the image you wanted me to see.",
                    author
                ),
                None,
            ))
        }
    };

    let hash = match hash_image(&url).await {
        Ok(hash) => hash,
        Err(err) => {
            tracing::warn!("could not decode image: {}", err);

            return Ok((
                format!(
                    "Hey {}, I couldn't find or understand the image you're asking about.",
                    author
                ),
                None,
            ));
        }
    };

    let results = crate::utils::lookup_single_hash(fuzzysearch, hash, Some(3), true).await?;
    tracing::info!("found {} results for image", results.len());

    let reply = if results.is_empty() {
        format!("Hey {}, I couldn't find any results, sorry.", author)
    } else if results.len() > 10 {
        format!("Hey {}, I found too many results to list!", author)
    } else {
        let mut reply = String::new();

        reply.push_str(&format!(
            "Hi {}, I found {} {} for this image.\n\n",
            author,
            results.len(),
            if results.len() == 1 {
                "result"
            } else {
                "results"
            },
        ));

        for result in results {
            let url = result.url();
            let display_text = url.replace("https://www.", "").replace("https://", "");

            reply.push_str(&format!("* [{}]({})\n", display_text, url));
        }

        reply
    };

    Ok((reply, Some(hash)))
}

async fn post_reply(
    reddit_client: &reqwest::Client,
    parent_id: &str,
    reply: String,
) -> Option<String> {
    let resp: serde_json::Value = reddit_client
        .post("https://oauth.reddit.com/api/comment")
        .form(&[
            ("api_type", "json"),
            ("text", &reply),
            ("parent", parent_id),
        ])
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp.get("json")
        .and_then(|obj| obj.get("data"))
        .and_then(|json| json.get("things"))
        .and_then(|things| things.as_array())
        .and_then(|things| things.first())
        .and_then(|thing| thing.get("data"))
        .and_then(|data| data.get("name"))
        .and_then(|name| name.as_str())
        .map(|s| s.to_string())
}

pub enum RedditJobQueue {
    Default,
}

impl JobQueue for RedditJobQueue {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "foxbot_reddit_default",
        }
    }

    fn priority_order() -> Vec<&'static str> {
        vec![Self::Default.as_str()]
    }
}

#[derive(Clone, Debug)]
pub struct LoadMentionsJob;

impl BotJob<RedditJobQueue> for LoadMentionsJob {
    const NAME: &'static str = "load_mentions";

    type JobData = ();

    fn queue(&self) -> RedditJobQueue {
        RedditJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        Ok(vec![])
    }

    fn deserialize(_args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessMentionJob {
    pub name: String,
    pub author: String,
    pub parent_id: Option<String>,
}

impl BotJob<RedditJobQueue> for ProcessMentionJob {
    const NAME: &'static str = "process_mention";

    type JobData = Self;

    fn queue(&self) -> RedditJobQueue {
        RedditJobQueue::Default
    }

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(vec![value])
    }

    fn deserialize(mut args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error> {
        serde_json::from_value(args.remove(0))
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RedditCommonInfo {
    pub name: String,
    pub author: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "data")]
pub enum RedditInfo {
    #[serde(rename = "t1")] // "Comment"
    Comment {
        #[serde(flatten)]
        common: RedditCommonInfo,
        link_id: Option<String>,
    },
    #[serde(rename = "t3")] // "Link"
    Link {
        #[serde(flatten)]
        common: RedditCommonInfo,
        url: String,
        crosspost_parent: Option<String>,
    },
}

async fn reddit_info(
    reddit_client: &reqwest::Client,
    id: &str,
) -> Result<roux::responses::BasicThing<roux::responses::Listing<RedditInfo>>, Error> {
    let data = reddit_client
        .get(format!("https://oauth.reddit.com/api/info/?id={}", id))
        .send()
        .await?
        .json()
        .await?;

    Ok(data)
}

async fn reddit_info_one(reddit_client: &reqwest::Client, id: &str) -> Result<RedditInfo, Error> {
    let mut info = reddit_info(reddit_client, id).await?;

    let item = info
        .data
        .children
        .pop()
        .ok_or_else(|| Error::missing("could not find id on reddit"))?;

    if !info.data.children.is_empty() {
        return Err(Error::user_message("more data was returned than expected"));
    }

    Ok(item)
}

async fn hash_image(url: &str) -> Result<i64, Error> {
    let check_bytes = crate::utils::CheckFileSize::new(url, 50_000_000);
    let bytes = check_bytes.into_bytes().await?;

    let hash = tokio::task::spawn_blocking(move || fuzzysearch::hash_bytes(&bytes)).await??;

    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_reddit_info() {
        let value = serde_json::json!({
            "kind": "t1",
            "data": {
                "name": "t1_asdf12",
                "author": "username",
                "body": "some text",
                "link_id": "t3_12asdf",
            }
        });

        let deserialized: RedditInfo = serde_json::from_value(value).unwrap();
        assert_eq!(
            deserialized,
            RedditInfo::Comment {
                common: RedditCommonInfo {
                    name: "t1_asdf12".to_string(),
                    author: "username".to_string(),
                },
                link_id: Some("t3_12asdf".to_string()),
            }
        );

        let value = serde_json::json!({
            "kind": "t3",
            "data": {
                "name": "t1_asdf12",
                "author": "username",
                "url": "https://syfaro.net"
            }
        });

        let deserialized: RedditInfo = serde_json::from_value(value).unwrap();
        assert_eq!(
            deserialized,
            RedditInfo::Link {
                common: RedditCommonInfo {
                    name: "t1_asdf12".to_string(),
                    author: "username".to_string(),
                },
                url: "https://syfaro.net".to_string(),
                crosspost_parent: None,
            }
        )
    }
}
