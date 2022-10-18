use std::{
    collections::HashMap,
    future::Future,
    net::TcpStream,
    sync::{Arc, Mutex},
};

use opentelemetry::propagation::TextMapPropagator;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use tracing::Instrument;

lazy_static::lazy_static! {
    static ref JOB_EXECUTION_TIME: HistogramVec = register_histogram_vec!("foxbot_job_duration_seconds", "Duration to complete a job.", &["job"]).unwrap();
    static ref JOB_FAILURE_COUNT: CounterVec = register_counter_vec!("foxbot_job_failure_total", "Number of job failures.", &["job"]).unwrap();
}

#[derive(Debug, thiserror::Error)]
pub enum FaktoryError {
    #[error("protocol error: {0}")]
    Protocol(#[from] faktory::Error),
    #[error("runtime error: {0}")]
    Runtime(#[from] tokio::task::JoinError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type JobCustom = HashMap<String, serde_json::Value>;

pub trait JobQueue {
    fn as_str(&self) -> &'static str;
    fn priority_order() -> Vec<&'static str>;
}

pub trait BotJob<Q>: Sized + std::fmt::Debug
where
    Q: JobQueue,
{
    const NAME: &'static str;

    type JobData;

    fn queue(&self) -> Q;

    fn args(self) -> Result<Vec<serde_json::Value>, serde_json::Error>;
    fn deserialize(args: Vec<serde_json::Value>) -> Result<Self::JobData, serde_json::Error>;
}

#[derive(Clone)]
pub struct FaktoryClient {
    client: Arc<Mutex<faktory::Producer<TcpStream>>>,
}

impl FaktoryClient {
    pub async fn connect<H: Into<String>>(host: H) -> Result<Self, FaktoryError> {
        let host = host.into();

        let producer = tokio::task::spawn_blocking(move || faktory::Producer::connect(Some(&host)))
            .in_current_span()
            .await??;

        let client = Arc::new(Mutex::new(producer));

        Ok(Self { client })
    }

    pub async fn enqueue_job<J, Q>(
        &self,
        job: J,
        extra: Option<JobCustom>,
    ) -> Result<String, FaktoryError>
    where
        J: BotJob<Q>,
        Q: JobQueue,
    {
        self.enqueue_job_at(job, extra, None).await
    }

    pub async fn enqueue_job_at<J, Q>(
        &self,
        job: J,
        extra: Option<JobCustom>,
        at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<String, FaktoryError>
    where
        J: BotJob<Q>,
        Q: JobQueue,
    {
        let name = J::NAME;
        let queue = job.queue().as_str();
        let args = job.args()?;

        tracing::trace!(
            "attempting to enqueue job {} on queue {} with args {:?}",
            name,
            queue,
            args
        );

        let mut job = faktory::Job::new(name, args).on_queue(queue);
        job.custom = Self::job_custom(extra.unwrap_or_default());
        job.at = at;

        let job_id = job.id().to_owned();

        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            let mut client = client.lock().expect("faktory client was poisoned");
            client.enqueue(job)
        })
        .in_current_span()
        .await??;

        tracing::info!(%job_id, "enqueued job {}", name);

        Ok(job_id)
    }

    fn job_custom(other: JobCustom) -> JobCustom {
        Self::tracing_headers()
            .into_iter()
            .map(|(key, value)| (key, serde_json::Value::from(value)))
            .chain(other)
            .collect()
    }

    fn tracing_headers() -> HashMap<String, String> {
        use tracing_opentelemetry::OpenTelemetrySpanExt;

        let mut headers = HashMap::with_capacity(2);
        let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();

        let cx = tracing::Span::current().context();
        propagator.inject_context(&cx, &mut headers);

        headers
    }
}

pub struct FaktoryWorkerEnvironment<C, E> {
    consumer_builder: faktory::ConsumerBuilder<E>,
    rt: tokio::runtime::Handle,

    context: C,
}

impl<C, E> FaktoryWorkerEnvironment<C, E>
where
    C: 'static + Send + Sync + Clone,
    E: std::fmt::Debug + std::error::Error + From<serde_json::Error> + crate::ErrorMetadata,
{
    pub fn new(context: C) -> Self {
        let consumer_builder = faktory::ConsumerBuilder::default();
        let rt = tokio::runtime::Handle::current();

        Self {
            consumer_builder,
            rt,
            context,
        }
    }

    pub fn register<J, Q, F, Fut>(&mut self, f: F)
    where
        J: BotJob<Q>,
        Q: JobQueue,
        F: 'static + Send + Sync + Fn(C, faktory::Job, J::JobData) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let name = J::NAME;

        let rt = self.rt.clone();
        let context = self.context.clone();

        let handler = move |job: faktory::Job| -> Result<(), E> {
            sentry::start_session();

            let custom_strings: HashMap<String, String> = job
                .custom
                .iter()
                .flat_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.to_owned(), value.to_owned()))
                })
                .collect();

            let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::new();
            let cx = propagator.extract(&custom_strings);

            let span =
                tracing::info_span!("faktory_job", name, queue = %job.queue, job_id = %job.id());
            tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, cx);

            let _guard = span.entered();

            tracing::info!("running job with args: {:?}", job.args());

            let data = J::deserialize(job.args().to_vec()).map_err(From::from)?;

            let execution_time = JOB_EXECUTION_TIME.with_label_values(&[name]).start_timer();
            let result = rt.block_on(f(context.clone(), job, data).in_current_span());
            let execution_time = execution_time.stop_and_record();

            let (result, status) = match result {
                Ok(_) => {
                    tracing::info!(execution_time, "job completed");
                    (Ok(()), sentry::protocol::SessionStatus::Exited)
                }
                Err(err) if !err.is_retryable() => {
                    JOB_FAILURE_COUNT.with_label_values(&[name]).inc();
                    tracing::warn!(
                        execution_time,
                        "job failed, and marked as not retryable: {:?}",
                        err
                    );
                    (Ok(()), sentry::protocol::SessionStatus::Abnormal)
                }
                Err(err) => {
                    JOB_FAILURE_COUNT.with_label_values(&[name]).inc();
                    tracing::error!(execution_time, "job failed, will retry: {:?}", err);
                    (Err(err), sentry::protocol::SessionStatus::Abnormal)
                }
            };

            sentry::end_session_with_status(status);

            result
        };

        self.consumer_builder.register(name, handler);
    }

    pub fn finalize(self) -> faktory::ConsumerBuilder<E> {
        self.consumer_builder
    }
}

#[macro_export]
macro_rules! extract_args {
    ($args:expr, $($x:ty),*) => {
        {
            (
                $(
                    $crate::services::faktory::get_arg::<$x>(&mut $args)?,
                )*
            )
        }
    }
}

#[macro_export]
macro_rules! serialize_args {
    ($($x:expr),*) => {
        {
            vec![
            $(
                serde_json::to_value($x)?,
            )*
            ]
        }
    }
}
