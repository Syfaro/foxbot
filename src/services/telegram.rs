use std::time::Duration;

use prometheus::{register_counter_vec, CounterVec};

lazy_static::lazy_static! {
    static ref TELEGRAM_REQUESTS: CounterVec = register_counter_vec!(
        "foxbot_telegram_requests_total",
        "Number of requests made to Telegram",
        &["endpoint"]
    )
    .unwrap();
    static ref TELEGRAM_ERRORS: CounterVec = register_counter_vec!(
        "foxbot_telegram_errors_total",
        "Number of errors returned on requests to Telegram",
        &["endpoint"]
    )
    .unwrap();
}

pub struct Telegram {
    tg: tgbotapi::Telegram,
}

impl Telegram {
    pub fn new(api_token: String) -> Self {
        Self {
            tg: tgbotapi::Telegram::new(api_token),
        }
    }

    pub async fn make_request<T>(&self, request: &T) -> Result<T::Response, tgbotapi::Error>
    where
        T: tgbotapi::TelegramRequest,
    {
        let mut attempts = 0;

        loop {
            TELEGRAM_REQUESTS
                .with_label_values(&[request.endpoint()])
                .inc();

            let err = match self.tg.make_request(request).await {
                Ok(resp) => return Ok(resp),
                Err(err) => err,
            };

            TELEGRAM_ERRORS
                .with_label_values(&[request.endpoint()])
                .inc();

            if attempts > 2 {
                return Err(err);
            }

            let retry_after = match err {
                tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                    parameters:
                        Some(tgbotapi::ResponseParameters {
                            retry_after: Some(retry_after),
                            ..
                        }),
                    ..
                }) => {
                    tracing::warn!(retry_after, "request was rate limited, retrying");
                    // Set a max for the retry_after time to avoid hanging.
                    std::cmp::min(retry_after, 15)
                }
                tgbotapi::Error::Telegram(tgbotapi::TelegramError {
                    error_code: Some(400),
                    description: Some(desc),
                    ..
                }) if desc
                    == "Bad Request: wrong file_id or the file is temporarily unavailable" =>
                {
                    tracing::warn!("file_id temporarily unavailable");
                    2
                }
                tgbotapi::Error::Request(err) => {
                    tracing::warn!("telegram network request error: {}", err);
                    2
                }
                err => {
                    tracing::warn!("got other telegram error: {}", err);
                    return Err(err);
                }
            };

            tokio::time::sleep(Duration::from_secs(retry_after as u64)).await;
            attempts += 1;
        }
    }

    pub async fn download_file(&self, file_path: &str) -> Result<Vec<u8>, tgbotapi::Error> {
        self.tg.download_file(file_path).await
    }
}
