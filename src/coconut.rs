use anyhow::Context;

pub struct Coconut {
    api_token: String,
    webhook: String,
    b2_account_id: String,
    b2_app_key: String,
    b2_bucket_id: String,

    client: reqwest::Client,
}

impl Coconut {
    const COCONUT_API_ENDPOINT: &'static str = "https://api.coconut.co/v1/jobs";

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

    pub async fn start_video(&self, source: &str, name: &str) -> anyhow::Result<i32> {
        let config = self.build_config(&source, &name);

        let resp = self
            .client
            .post(Self::COCONUT_API_ENDPOINT)
            .basic_auth(&self.api_token, None::<&str>)
            .body(config)
            .send()
            .await?;

        let status = resp.status();

        if status != reqwest::StatusCode::CREATED {
            let text = resp.text().await?;
            tracing::error!("Coconut error: {:?}", text);
            return Err(anyhow::anyhow!("Unable to create encode job"));
        }

        let json: serde_json::Value = resp.json().await?;
        tracing::trace!("Sent encode job: {:?}", json);

        let id = json
            .get("id")
            .context("Coconut response missing ID")?
            .as_i64()
            .context("Coconut response ID was not number")?;
        Ok(id as i32)
    }

    fn build_config(&self, source: &str, name: &str) -> String {
        format!(
            "
            var account_id = {account_id}
            var app_key = {app_key}
            var bucket_id = {bucket_id}
            var cdn = b2://$account_id:$app_key@$bucket_id

            # Settings
            set source = {source}
            set webhook = {webhook}?id={name}, events=true

            # Outputs
            -> mp4:720p = $cdn/video/{name}.mp4, if=$source_duration <= 60
            -> mp4:480p = $cdn/video/{name}.mp4, if=$source_duration <= 120 AND $source_duration > 60
            -> mp4:360p = $cdn/video/{name}.mp4, if=$source_duration > 120, duration = 200
            -> jpg:250x0 = $cdn/thumbnail/{name}.jpg, number=1
        ",
            account_id = self.b2_account_id,
            app_key = self.b2_app_key,
            bucket_id = self.b2_bucket_id,
            source = source,
            webhook = self.webhook,
            name = name
        )
    }
}
