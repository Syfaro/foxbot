pub struct Coconut {
    api_token: String,
    webhook: String,
    s3_endpoint: String,
    s3_token: String,
    s3_secret: String,
    s3_bucket: String,

    client: reqwest::Client,
}

impl Coconut {
    const COCONUT_API_ENDPOINT: &'static str = "https://api.coconut.co/v1/jobs";

    pub fn new(
        api_token: String,
        webhook: String,
        s3_endpoint: String,
        s3_token: String,
        s3_secret: String,
        s3_bucket: String,
    ) -> Self {
        let client = reqwest::Client::new();

        Self {
            api_token,
            webhook,
            s3_endpoint,
            s3_token,
            s3_secret,
            s3_bucket,

            client,
        }
    }

    pub async fn start_video(&self, source: &str, name: &str) -> anyhow::Result<()> {
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

        Ok(())
    }

    fn build_config(&self, source: &str, name: &str) -> String {
        format!(
            "
            var access_key = {access}
            var secret_key = {secret}
            var bucket = {bucket}
            var host = {host}
            var cdn = s3://$access_key:$secret_key@$bucket

            # Settings
            set source = {source}
            set webhook = {webhook}?id={name}

            # Outputs
            -> mp4:720p = $cdn/video/{name}.mp4?host=$host
            -> jpg:250x0 = $cdn/thumbnail/{name}.jpg?host=$host, number=1
        ",
            access = self.s3_token,
            secret = self.s3_secret,
            bucket = self.s3_bucket,
            host = self.s3_endpoint,
            source = source,
            webhook = self.webhook,
            name = name
        )
    }
}
