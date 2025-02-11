use sentry_tower::NewSentryLayer;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use tracing_subscriber::prelude::*;

use apalis::{
    layers::{SentryJobLayer, TraceLayer},
    prelude::*,
    redis::RedisStorage,
};
use tokio::time::sleep;

use email_service::Email;

#[derive(Debug)]
struct InvalidEmailError {
    email: String,
}

impl fmt::Display for InvalidEmailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid Email: {} is not a valid email", self.email)
    }
}

impl Error for InvalidEmailError {}

macro_rules! update_progress {
    ($bread_crumb:expr, $progress:expr) => {
        tracing::info!(progress = ?$progress, $bread_crumb);
    };
}

async fn email_service(email: Email, _ctx: JobContext) -> Result<JobResult, JobError> {
    let parent_span = sentry::configure_scope(|scope| scope.get_span());

    let tx_ctx =
        sentry::TransactionContext::continue_from_span("email.send", "apalis.job", parent_span);
    let transaction = sentry::start_transaction(tx_ctx);
    sentry::configure_scope(|scope| scope.set_span(Some(transaction.clone().into())));

    // Start Check Dns Span
    {
        let dns_span = transaction.start_child("dns", "Checking if dns configured");

        update_progress!("Checking if dns configured", 10);
        sleep(Duration::from_millis(1008)).await;
        update_progress!("Found dns config", 20);

        dns_span.finish();
    }
    // End Dns Span

    // Start fetch Sendgrid details
    {
        let send_grid_span = transaction.start_child("sendgrid", "Getting sendgrid details");

        tracing::info!("Getting sendgrid details");
        sleep(Duration::from_millis(712)).await;
        update_progress!("Found Sendgrid details", 30);

        send_grid_span.finish();
    }
    // End Sendgrid Span

    // Fetch user details
    {
        let user_span = transaction.start_child("user", "Fetching user details");
        tracing::info!("Fetching user details");
        sleep(Duration::from_millis(100)).await;
        update_progress!("Found user", 50);
        {
            let user_deeper_span = transaction.start_child("user.deeper", "Fetching from Database");
            tracing::warn!("Digging deeper");
            sleep(Duration::from_millis(209)).await;
            user_deeper_span.finish();
        }
        {
            let user_by_id_span = transaction.start_child("user.by_id", "Trying to fetch by id");
            sleep(Duration::from_millis(120)).await;
            // Record some error
            // let err = email.to.parse::<usize>().unwrap_err();
            // sentry::capture_error(&err);
            user_by_id_span.finish();
        }
        user_span.finish();
    }

    tracing::warn!("Failed. Email is not valid");
    transaction.finish();
    Err(JobError::Failed(Box::new(InvalidEmailError {
        email: email.to,
    })))
}

async fn produce_jobs(mut storage: RedisStorage<Email>) {
    storage
        .push(Email {
            to: "apalis@example".to_string(),
            text: "Test backround job from Apalis".to_string(),
            subject: "Welcome Sentry Email".to_string(),
        })
        .await
        .unwrap();
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    use tracing_subscriber::EnvFilter;
    std::env::set_var("RUST_LOG", "debug");
    let sentry_dsn =
        std::env::var("SENTRY_DSN").expect("Please set SENTRY_DSN environmental variable");
    let redis_url =
        std::env::var("REDIS_URL").expect("Please set REDIS_URL environmental variable");
    let _guard = sentry::init((
        sentry_dsn,
        sentry::ClientOptions {
            release: sentry::release_name!(),
            traces_sample_rate: 0.2,
            ..Default::default()
        },
    ));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("debug"))
        .unwrap();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(sentry_tracing::layer())
        .init();

    let storage = RedisStorage::connect(redis_url)
        .await
        .expect("Could not connect to RedisStorage");
    //This can be in another part of the program
    produce_jobs(storage.clone()).await;

    Monitor::new()
        .register_with_count(2, move |_| {
            WorkerBuilder::new(storage.clone())
                .layer(NewSentryLayer::new_from_top())
                .layer(SentryJobLayer::new())
                .layer(TraceLayer::new())
                .build_fn(email_service)
        })
        .run()
        .await
}
