use actix_web::{web, App, HttpResponse, HttpServer};
use apalis::prelude::*;
use apalis::{layers::TraceLayer, redis::RedisStorage};
use futures::future;

use email_service::{send_email, Email};

async fn push_email(
    email: web::Json<Email>,
    storage: web::Data<RedisStorage<Email>>,
) -> HttpResponse {
    let storage = &*storage.into_inner();
    let mut storage = storage.clone();
    let res = storage.push(email.into_inner()).await;
    match res {
        Ok(job_id) => HttpResponse::Ok().body(format!("Email added to queue: {job_id}")),
        Err(e) => HttpResponse::InternalServerError().body(format!("{}", e)),
    }
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "debug");
    env_logger::init();

    let storage = RedisStorage::connect("redis://127.0.0.1/").await.unwrap();
    let data = web::Data::new(storage.clone());
    let http = HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .service(web::scope("/emails").route("/push", web::post().to(push_email)))
    })
    .bind("127.0.0.1:8000")?
    .run();
    let worker = Monitor::new()
        .register_with_count(2, move |_| {
            WorkerBuilder::new(storage.clone())
                .layer(TraceLayer::new())
                .build_fn(send_email)
        })
        .run();

    future::try_join(http, worker).await?;
    Ok(())
}
