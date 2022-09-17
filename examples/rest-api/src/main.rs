use std::collections::HashSet;
use std::time::Duration;

use actix_cors::Cors;
use actix_web::{web, App, HttpResponse, HttpServer, Scope};
use apalis::{
    layers::{SentryJobLayer, TraceLayer},
    mysql::MysqlStorage,
    postgres::PostgresStorage,
    prelude::*,
    redis::RedisStorage,
    sqlite::SqliteStorage,
};
use futures::future;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use email_service::{send_email, Email};

#[derive(Debug, Deserialize, Serialize)]
struct Notification {
    text: String,
}

impl Job for Notification {
    const NAME: &'static str = "sqlite::Notification";
}

async fn notification_service(
    notif: Notification,
    _ctx: JobContext,
) -> Result<JobResult, JobError> {
    println!("Attempting to send notification {}", notif.text);
    tokio::time::sleep(Duration::from_millis(1)).await;
    Ok(JobResult::Success)
}

#[derive(Debug, Deserialize, Serialize)]
struct Document {
    text: String,
}

impl Job for Document {
    const NAME: &'static str = "postgres::Document";
}

async fn document_service(doc: Document, _ctx: JobContext) -> Result<JobResult, JobError> {
    println!("Attempting to convert {} to pdf", doc.text);
    tokio::time::sleep(Duration::from_millis(1)).await;
    Ok(JobResult::Success)
}

#[derive(Debug, Deserialize, Serialize)]
struct Upload {
    url: String,
}

impl Job for Upload {
    const NAME: &'static str = "mysql::Upload";
}

async fn upload_service(upload: Upload, _ctx: JobContext) -> Result<JobResult, JobError> {
    println!("Attempting to upload {} to cloud", upload.url);
    tokio::time::sleep(Duration::from_millis(1)).await;
    Ok(JobResult::Success)
}

#[derive(Serialize)]
struct JobsResult<J> {
    jobs: Vec<JobRequest<J>>,
    counts: Counts,
}
#[derive(Deserialize)]
struct Filter {
    #[serde(default)]
    status: JobState,
    #[serde(default)]
    page: i32,
}

#[derive(Deserialize)]
struct JobId {
    job_id: String,
}

async fn push_job<J, S>(job: web::Json<J>, storage: web::Data<S>) -> HttpResponse
where
    J: Job + Serialize + DeserializeOwned + 'static,
    S: Storage<Output = J>,
{
    let storage = &*storage.into_inner();
    let mut storage = storage.clone();
    let res = storage.push(job.into_inner()).await;
    match res {
        Ok(job_id) => HttpResponse::Ok().body(format!("Job added to queue: {job_id}")),
        Err(e) => HttpResponse::InternalServerError().body(format!("{}", e)),
    }
}

async fn get_jobs<J, S>(storage: web::Data<S>, filter: web::Query<Filter>) -> HttpResponse
where
    J: Job + Serialize + DeserializeOwned + 'static,
    S: Storage<Output = J> + JobStreamExt<J> + Send,
{
    let storage = &*storage.into_inner();
    let mut storage = storage.clone();
    let counts = storage.counts().await.unwrap();
    let jobs = storage.list_jobs(&filter.status, filter.page).await;

    match jobs {
        Ok(jobs) => HttpResponse::Ok().json(JobsResult { jobs, counts }),
        Err(e) => HttpResponse::InternalServerError().body(format!("{}", e)),
    }
}

async fn get_workers<J, S>(storage: web::Data<S>) -> HttpResponse
where
    J: Job + Serialize + DeserializeOwned + 'static,
    S: Storage<Output = J> + JobStreamExt<J>,
{
    let storage = &*storage.into_inner();
    let mut storage = storage.clone();
    let workers = storage.list_workers().await;
    match workers {
        Ok(workers) => HttpResponse::Ok().json(serde_json::to_value(workers).unwrap()),
        Err(e) => HttpResponse::InternalServerError().body(format!("{}", e)),
    }
}

async fn get_job<J, S>(job: web::Path<JobId>, storage: web::Data<S>) -> HttpResponse
where
    J: Job + Serialize + DeserializeOwned + 'static,
    S: Storage<Output = J> + 'static,
{
    let storage = &*storage.into_inner();
    let storage = storage.clone();
    let res = storage.fetch_by_id(job.job_id.to_string()).await;
    match res {
        Ok(Some(job)) => HttpResponse::Ok().json(job),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::InternalServerError().body(format!("{}", e)),
    }
}

trait StorageRest<J>: Storage<Output = J> {
    fn name(&self) -> String;
}

impl<J, S> StorageRest<J> for S
where
    S: Storage<Output = J> + JobStreamExt<J> + 'static,
    J: Job + Serialize + DeserializeOwned + 'static,
{
    fn name(&self) -> String {
        J::NAME.to_string()
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Queue {
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct QueueList {
    set: HashSet<String>,
}

struct StorageApiBuilder {
    scope: Scope,
    list: QueueList,
}

impl StorageApiBuilder {
    fn add_storage<J, S>(mut self, storage: S) -> Self
    where
        J: Job + Serialize + DeserializeOwned + 'static,
        S: StorageRest<J> + JobStreamExt<J>,
        S: Storage<Output = J>,
        S: 'static + Send,
    {
        let name = J::NAME.to_string();
        self.list.set.insert(name);

        Self {
            scope: self.scope.service(
                Scope::new(J::NAME)
                    .app_data(web::Data::new(storage))
                    .route("", web::get().to(get_jobs::<J, S>)) // Fetch jobs in queue
                    .route("/workers", web::get().to(get_workers::<J, S>)) // Fetch jobs in queue
                    .route("/job", web::put().to(push_job::<J, S>)) // Allow add jobs via api
                    .route("/job/{job_id}", web::get().to(get_job::<J, S>)), // Allow fetch specific job
            ),
            list: self.list,
        }
    }

    fn build(self) -> Scope {
        async fn fetch_queues(queues: web::Data<QueueList>) -> HttpResponse {
            let mut queue_result = Vec::new();
            for queue in &queues.set {
                queue_result.push(Queue {
                    name: queue.clone(),
                })
            }
            #[derive(Serialize)]
            struct Res {
                queues: Vec<Queue>,
            }

            HttpResponse::Ok().json(Res {
                queues: queue_result,
            })
        }

        self.scope
            .app_data(web::Data::new(self.list))
            .route("", web::get().to(fetch_queues))
    }

    fn new() -> Self {
        Self {
            scope: Scope::new("queues"),
            list: QueueList {
                set: HashSet::new(),
            },
        }
    }
}

async fn produce_redis_jobs(mut storage: RedisStorage<Email>) {
    for i in 0..10 {
        storage
            .push(Email {
                to: format!("test{}@example.com", i),
                text: "Test backround job from Apalis".to_string(),
                subject: "Background email job".to_string(),
            })
            .await
            .unwrap();
    }
}
async fn produce_sqlite_jobs(mut storage: SqliteStorage<Notification>) {
    for i in 0..100 {
        storage
            .push(Notification {
                text: format!("Notiification: {}", i),
            })
            .await
            .unwrap();
    }
}

async fn produce_postgres_jobs(mut storage: PostgresStorage<Document>) {
    for i in 0..100 {
        storage
            .push(Document {
                text: format!("Document: {}", i),
            })
            .await
            .unwrap();
    }
}

async fn produce_mysql_jobs(mut storage: MysqlStorage<Upload>) {
    for i in 0..100 {
        storage
            .push(Upload {
                url: format!("Upload: {}", i),
            })
            .await
            .unwrap();
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "debug,sqlx::query=error");
    env_logger::init();
    let database_url = std::env::var("DATABASE_URL").expect("Must specify DATABASE_URL");
    let pg: PostgresStorage<Document> = PostgresStorage::connect(database_url).await.unwrap();
    let _res = pg.setup().await.expect("Unable to migrate");

    let database_url = std::env::var("MYSQL_URL").expect("Must specify MYSQL_URL");

    let mysql: MysqlStorage<Upload> = MysqlStorage::connect(database_url).await.unwrap();
    mysql
        .setup()
        .await
        .expect("unable to run migrations for mysql");

    let storage = RedisStorage::connect("redis://127.0.0.1/").await.unwrap();

    let sqlite = SqliteStorage::connect("sqlite://data.db").await.unwrap();
    let _res = sqlite.setup().await.expect("Unable to migrate");

    let worker_storage = storage.clone();
    let sqlite_storage = sqlite.clone();
    let pg_storage = pg.clone();
    let mysql_storage = mysql.clone();

    produce_redis_jobs(storage.clone()).await;
    produce_sqlite_jobs(sqlite.clone()).await;
    produce_postgres_jobs(pg_storage.clone()).await;
    produce_mysql_jobs(mysql.clone()).await;
    let http = HttpServer::new(move || {
        App::new().wrap(Cors::permissive()).service(
            web::scope("/api").service(
                StorageApiBuilder::new()
                    .add_storage(storage.clone())
                    .add_storage(sqlite.clone())
                    .add_storage(pg.clone())
                    .add_storage(mysql.clone())
                    .build(),
            ),
        )
    })
    .bind("127.0.0.1:8000")?
    .run();

    let worker = Monitor::new()
        .register_with_count(1, move |_| {
            WorkerBuilder::new(worker_storage.clone())
                .layer(SentryJobLayer)
                .layer(TraceLayer::new())
                .build_fn(send_email)
        })
        .register_with_count(4, move |_| {
            WorkerBuilder::new(sqlite_storage.clone())
                .layer(SentryJobLayer)
                .layer(TraceLayer::new())
                .build_fn(notification_service)
        })
        .register_with_count(2, move |_| {
            WorkerBuilder::new(pg_storage.clone())
                .layer(SentryJobLayer)
                .layer(TraceLayer::new())
                .build_fn(document_service)
        })
        .register_with_count(2, move |_| {
            WorkerBuilder::new(mysql_storage.clone())
                .layer(SentryJobLayer)
                .layer(TraceLayer::new())
                .build_fn(upload_service)
        })
        .run();
    future::try_join(http, worker).await?;

    Ok(())
}
