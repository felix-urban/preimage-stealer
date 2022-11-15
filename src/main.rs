// This program accepts four arguments: host, port, cert file, macaroon file

extern crate core;

mod config;
mod memory;
mod redis;
mod sled;
mod storage;
mod subscribers;

use axum::response::Html;
use axum::routing::get;
use axum::{Extension, Router};
use clap::Parser;
use dioxus::prelude::*;
use std::sync::Arc;

use crate::memory::MemoryStorage;
use crate::redis::RedisStorage;
use crate::sled::SledStorage;

use crate::config::*;
use crate::storage::Storage;
use crate::subscribers::*;
use tokio::sync::Mutex;
use tokio::task::spawn;

#[tokio::main]
async fn main() {
    let config: Config = Config::parse();
    let config_clone = config.clone();

    let cert_file = config.cert_file.unwrap_or_else(default_cert_file);
    let macaroon_file = config
        .macaroon_file
        .unwrap_or_else(|| default_macaroon_file(config.network));

    // Connecting to LND requires only host, port, cert file, macaroon file
    let mut client = tonic_openssl_lnd::connect(config.host, config.port, cert_file, macaroon_file)
        .await
        .expect("failed to connect");
    let client_router = client.router().clone();

    let storage = load_storage(config_clone);

    // HTLC event stream part
    println!("starting htlc event subscription");
    let client_router_htlc_event = client_router.clone();
    let storage_htlc_event = storage.clone();

    spawn(async move {
        start_htlc_event_subscription(client_router_htlc_event, storage_htlc_event).await
    });

    println!("started htlc event subscription");

    // HTLC interceptor part
    println!("starting HTLC interception");
    let storage_htlc_interceptor = storage.clone();

    spawn(async move { start_htlc_interceptor(client_router, storage_htlc_interceptor).await });

    println!("started htlc event interception");

    let stolen = storage.lock().await.total_stolen();
    println!("current amount stolen: {stolen} msats");

    // TODO make port configurable
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("listening on http://{}", addr);

    let router = Router::new()
        .route("/", get(index))
        .route("/stolen", get(get_stolen))
        .layer(Extension(storage));

    axum::Server::bind(&addr)
        .serve(router.into_make_service())
        .await
        .unwrap();
}

fn load_storage(cfg: Config) -> Arc<Mutex<dyn Storage + Send>> {
    match cfg.database {
        // if no database type is defined, check the db-path and redis-url configs
        // set database config based on those, otherwise use memory db
        None => match cfg.db_path {
            Some(db_path) => Arc::new(Mutex::new(
                SledStorage::new(db_path.as_str()).expect("Failed to create sled storage"),
            )),
            None => match cfg.redis_url {
                Some(redis_url) => Arc::new(Mutex::new(
                    RedisStorage::new(redis_url.as_str()).expect("Failed to create redis storage"),
                )),
                None => Arc::new(Mutex::new(MemoryStorage::new())),
            },
        },
        // if a database type is set, use that type with provided config if available
        // error if conflicting database configurations are given
        Some(database) => {
            match database.to_lowercase().as_str() {
                "memory" => {
                    // these configs should not be set
                    if cfg.redis_url.is_some() {
                        panic!("redis-url cannot be set for memory db")
                    }
                    if cfg.db_path.is_some() {
                        panic!("db-path cannot be set for memory db")
                    }
                    Arc::new(Mutex::new(MemoryStorage::new()))
                }
                "sled" => {
                    // these configs should not be set
                    if cfg.redis_url.is_some() {
                        panic!("redis-url cannot be set for sled db")
                    }
                    match cfg.db_path {
                        Some(db_path) => Arc::new(Mutex::new(
                            SledStorage::new(db_path.as_str())
                                .expect("Failed to create sled storage"),
                        )),
                        None => Arc::new(Mutex::new(SledStorage::default())),
                    }
                }
                "redis" => {
                    // these configs should not be set
                    if cfg.db_path.is_some() {
                        panic!("db-path cannot be set for redis db")
                    }

                    match cfg.redis_url {
                        Some(redis_url) => Arc::new(Mutex::new(
                            RedisStorage::new(redis_url.as_str())
                                .expect("Failed to create redis storage"),
                        )),
                        None => Arc::new(Mutex::new(RedisStorage::default())),
                    }
                }
                _ => panic!("Failed to parse database type"),
            }
        }
    }
}

async fn index(Extension(stolen): Extension<Arc<Mutex<dyn Storage + Send>>>) -> Html<String> {
    let amt = stolen.lock().await.total_stolen();

    Html(dioxus::ssr::render_lazy(rsx! {
            h1 { "Total stolen: {amt} msats" }
    }))
}

async fn get_stolen(Extension(stolen): Extension<Arc<Mutex<dyn Storage + Send>>>) -> String {
    let amt = stolen.lock().await.total_stolen();
    amt.to_string()
}
