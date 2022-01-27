use crate::Error;

pub struct Coconut {
    api_token: String,
    webhook: String,
    b2_account_id: String,
    b2_app_key: String,
    b2_bucket_id: String,

    client: reqwest::Client,
}

impl Coconut {
    const COCONUT_API_ENDPOINT: &'static str = "https://api.coconut.co/v2/jobs";

    pub fn new(
        api_token: String,
        webhook: String,
        b2_account_id: String,
        b2_app_key: String,
        b2_bucket_id: String,
    ) -> Self {
        let client = reqwest::Client::new();

        Self {
            api_token,
            webhook,
            b2_account_id,
            b2_app_key,
            b2_bucket_id,

            client,
        }
    }

    pub async fn start_video(&self, source: &str, name: &str) -> Result<String, Error> {
        let config = self.build_config(source, name);

        let resp = self
            .client
            .post(Self::COCONUT_API_ENDPOINT)
            .basic_auth(&self.api_token, None::<&str>)
            .json(&config)
            .send()
            .await?;

        let status = resp.status();

        if status != reqwest::StatusCode::CREATED {
            let text = resp.text().await?;
            return Err(Error::bot(format!(
                "encode job could not be started: {}",
                text
            )));
        }

        let json: serde_json::Value = resp.json().await?;
        tracing::trace!("Sent encode job: {:?}", json);

        let id = json
            .get("id")
            .ok_or(Error::missing("job id"))?
            .as_str()
            .ok_or(Error::missing("job id as str"))?;

        Ok(id.to_owned())
    }

    fn build_config(&self, source: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "input": {
                "url": source,
            },
            "storage": {
                "service": "backblaze",
                "bucket_id": self.b2_bucket_id,
                "credentials": {
                    "app_key_id": self.b2_account_id,
                    "app_key": self.b2_app_key,
                }
            },
            "notification": {
                "type": "http",
                "url": self.webhook,
                "events": true,
                "metadata": true,
                "params": {
                    "name": name,
                }
            },
            "outputs": {
                "jpg:250x0": {
                    "path": format!("/thumbnail/{}.jpg", name),
                },
                "mp4:1080p": {
                    "path": format!("/video/{}.mp4", name),
                    "if": "{{ input.duration }} <= 60",
                },
                "mp4:720p": {
                    "path": format!("/video/{}.mp4", name),
                    "if": "{{ input.duration }} <= 120 and {{ input.duration }} > 60",
                },
                "mp4:480p": {
                    "path": format!("/video/{}.mp4", name),
                    "if": "{{ input.duration }} <= 180 and {{ input.duration }} > 120",
                },
                "mp4:360p": {
                    "path": format!("/video/{}.mp4", name),
                    "if": "{{ input.duration }} > 180"
                }
            }
        })
    }
}
