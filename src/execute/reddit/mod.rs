use std::sync::Arc;

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

    let reddit_client = reqwest::ClientBuilder::default()
        .default_headers(headers)
        .build()
        .unwrap();

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

    let fuzzysearch = fuzzysearch::FuzzySearch::new(config.fuzzysearch_api_token);

    let cx = Arc::new(Context {
        faktory,

        fuzzysearch,

        reddit: me,
        reddit_client,
    });

    let mut faktory_environment: FaktoryWorkerEnvironment<_, Error> =
        FaktoryWorkerEnvironment::new(cx.clone());

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

            let url = match reddit_info_one(&cx.reddit_client, &parent_id).await? {
                RedditInfo::Comment {
                    link_id: Some(link_id),
                    ..
                } => {
                    let url = match reddit_info_one(&cx.reddit_client, &link_id).await? {
                        RedditInfo::Comment { .. } => {
                            tracing::warn!("link_id returned comment, skipping");
                            return Ok(());
                        }
                        RedditInfo::Link { url, .. } => url,
                    };

                    tracing::debug!("got comment, want to find matches for url: {}", url);

                    url
                }
                RedditInfo::Link { url, .. } => {
                    tracing::debug!("got link, want to find matches for: {}", url);

                    url
                }
                _ => {
                    tracing::warn!("missing reddit data to process mention");

                    return Ok(());
                }
            };

            let hash = match hash_image(&url).await {
                Ok(hash) => hash,
                Err(err) => {
                    tracing::error!("could not hash image: {}", err);
                    return Ok(());
                }
            };

            let results = crate::utils::lookup_single_hash(&cx.fuzzysearch, hash, Some(3)).await?;
            tracing::info!("found {} results for image", results.len());

            let mut reply = String::new();

            if results.is_empty() {
                reply = format!(
                    "Hey {}, I couldn't find any results, sorry.",
                    mention_data.author
                );
            } else {
                reply.push_str(&format!(
                    "Hi {}, I found {} {} for this image.\n\n",
                    mention_data.author,
                    results.len(),
                    if results.len() == 1 {
                        "result"
                    } else {
                        "results"
                    },
                ));

                for result in results {
                    reply.push_str(&format!("* {}\n", result.url()));
                }
            }

            reply.push_str("\n\n_This comment was generated by a bot._");

            cx.reddit.comment(&reply, &mention_data.name).await?;
            tracing::info!("posted comment");

            Ok(())
        },
    );

    let mut environment = faktory_environment.finalize();
    environment.labels(vec!["foxbot-reddit".to_string()]);

    let faktory = environment.connect(Some(&config.faktory_url)).unwrap();

    faktory.run_to_completion(&RedditJobQueue::priority_order());
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

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct RedditCommonInfo {
    pub name: String,
    pub author: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
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
        .ok_or(Error::missing("could not find id on reddit"))?;

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
            }
        )
    }
}
