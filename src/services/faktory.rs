use std::{
    collections::HashMap,
    future::Future,
    net::TcpStream,
    sync::{Arc, Mutex},
};

use opentelemetry::propagation::TextMapPropagator;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use tracing::Instrument;

use crate::Error;

lazy_static::lazy_static! {
    static ref JOB_EXECUTION_TIME: HistogramVec = register_histogram_vec!("foxbot_job_duration_seconds", "Duration to complete a job.", &["job"]).unwrap();
    static ref JOB_FAILURE_COUNT: CounterVec = register_counter_vec!("foxbot_job_failure_total", "Number of job failures.", &["job"]).unwrap();
}

#[derive(Debug, thiserror::Error)]
pub enum FaktoryError {
    #[error("protocol error: {0}")]
    Protocol(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("runtime error: {0}")]
    Runtime(#[from] tokio::task::JoinError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type JobCustom = HashMap<String, serde_json::Value>;

#[derive(Clone)]
pub struct FaktoryClient {
    client: Arc<Mutex<faktory::Producer<TcpStream>>>,
}

impl FaktoryClient {
    pub async fn connect<H: Into<String>>(host: H) -> Result<Self, FaktoryError> {
        let host = host.into();

        let producer = tokio::task::spawn_blocking(move || faktory::Producer::connect(Some(&host)))
            .in_current_span()
            .await?
            .map_err(|err| {
                let err = err.compat();
                FaktoryError::Protocol(Box::new(err))
            })?;

        let client = Arc::new(Mutex::new(producer));

        Ok(Self { client })
    }

    pub async fn enqueue_job(
        &self,
        mut job: faktory::Job,
        extra: Option<JobCustom>,
    ) -> Result<(), FaktoryError> {
        job.custom = Self::job_custom(extra.unwrap_or_default());

        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            let mut client = client.lock().expect("faktory client was poisoned");
            client.enqueue(job)
        })
        .in_current_span()
        .await?
        .map_err(|err| {
            let err = err.compat();
            FaktoryError::Protocol(Box::new(err))
        })?;

        Ok(())
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
    E: std::fmt::Debug + std::error::Error + crate::ErrorMetadata,
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

    pub fn register<F, Fut>(&mut self, name: &'static str, f: F)
    where
        F: 'static + Send + Sync + Fn(C, faktory::Job) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
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

            let span = tracing::info_span!("faktory_job", name, queue = %job.queue);
            tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, cx);

            let _guard = span.entered();

            tracing::info!("running job with args: {:?}", job.args());

            let execution_time = JOB_EXECUTION_TIME.with_label_values(&[name]).start_timer();
            let result = rt.block_on(f(context.clone(), job).in_current_span());
            let execution_time = execution_time.stop_and_record();

            let (result, status) = match result {
                Ok(_) => {
                    tracing::info!(execution_time, "job completed");
                    (Ok(()), sentry::protocol::SessionStatus::Exited)
                }
                Err(err) if !err.is_retryable() => {
                    JOB_FAILURE_COUNT.with_label_values(&[name]).inc();
                    tracing::warn!(execution_time, "job failed, and marked as not retryable");
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

pub fn get_arg_opt<T: serde::de::DeserializeOwned>(
    args: &mut core::slice::Iter<serde_json::Value>,
) -> Result<Option<T>, Error> {
    let arg = match args.next() {
        Some(arg) => arg,
        None => return Ok(None),
    };

    let data = serde_json::from_value(arg.to_owned())?;
    Ok(Some(data))
}

pub fn get_arg<T: serde::de::DeserializeOwned>(
    args: &mut core::slice::Iter<serde_json::Value>,
) -> Result<T, Error> {
    get_arg_opt(args)?.ok_or(Error::Missing)
}

#[macro_export]
macro_rules! extract_args {
    ($args:expr, $($x:ty),*) => {
        {
            (
                $(
                    crate::services::faktory::get_arg::<$x>(&mut $args)?,
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
