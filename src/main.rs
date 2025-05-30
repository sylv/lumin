use axum::Router;
use cache::Cache;
use config::get_config;
use debrid::Debrid;
use extractor::start_duration_extractor;
use fuse::LuminFS;
use fuse3::raw::prelude::*;
use fuse3::{MountOptions, raw::MountHandle};
use qbittorrent::mimic_qbittorrent;
use reconciler::start_reconciler;
use rpc::get_rpc_router;
use sea_orm::DatabaseConnection;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use std::{
    env,
    str::FromStr,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::{net::TcpListener, signal, sync::Notify, task::JoinHandle};
use tracing::{debug, info};

mod cache;
mod config;
mod debrid;
mod entities;
mod error;
mod extractor;
mod fuse;
mod helpers;
mod qbittorrent;
mod reconciler;
mod rpc;

pub struct AppState {
    pub db: DatabaseConnection,
    pub debrid: Arc<Debrid>,
    pub notifier: Arc<Notify>,
    pub is_scanning: Arc<AtomicBool>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger
    tracing_subscriber::fmt::init();
    dotenv::dotenv().ok();

    // Read configuration
    let config = config::get_config();
    let db_path = config.data_dir.join("data.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(300))
        .connect_with(
            // https://briandouglas.ie/sqlite-defaults/
            SqliteConnectOptions::from_str(db_path.to_string_lossy().as_ref())
                .expect("Failed to parse SQLite path")
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .busy_timeout(Duration::from_secs(5))
                .foreign_keys(true)
                .auto_vacuum(SqliteAutoVacuum::Incremental)
                .pragma("cache_size", "-10000")
                .pragma("temp_store", "MEMORY")
                .create_if_missing(true)
                .page_size(8192),
        )
        .await
        .expect("Failed to connect to SQLite");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    let db = DatabaseConnection::from(pool);

    let debrid = Arc::new(Debrid::new());
    let notifier = Arc::new(Notify::new());
    let is_scanning = Arc::new(AtomicBool::new(false));
    let config = get_config();

    let reconciler_handle = tokio::spawn({
        let db = db.clone();
        let debrid = debrid.clone();
        let notifier = notifier.clone();
        let is_scanning = is_scanning.clone();

        async move {
            start_reconciler(&db, debrid, notifier, is_scanning)
                .await
                .expect("Failed to start reconciler");
        }
    });

    let (cache, cache_handle) = {
        let debrid = debrid.clone();
        let db = db.clone();
        let cache = Cache::load(&db, debrid)
            .await
            .expect("Failed to load cache");

        let cache_handle = tokio::spawn({
            let cache = cache.clone();
            async move {
                cache
                    .start_sweeper()
                    .await
                    .expect("Failed to start cache sweeper")
            }
        });

        (cache, cache_handle)
    };

    let mount_handle = {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        let mut mount_options = MountOptions::default();
        mount_options
            .fs_name("lumin")
            .force_readdir_plus(true)
            .nonempty(true)
            .allow_other(config.allow_other)
            .read_only(false)
            .no_open_dir_support(true)
            .no_open_support(true)
            .uid(uid)
            .gid(gid);

        let cache = cache.clone();
        let fs = LuminFS::new(db.clone(), cache);
        let session = Session::new(mount_options);
        if config.mount_unprivileged {
            session
                .mount_with_unprivileged(fs, &config.mount_path)
                .await
                .unwrap()
        } else {
            session.mount(fs, &config.mount_path).await.unwrap()
        }
    };

    let extractor_handle = {
        let db = db.clone();
        let cache = cache.clone();

        tokio::spawn(async move {
            start_duration_extractor(db, cache)
                .await
                .expect("Failed to start duration extractor");
        })
    };

    let state = Arc::new(AppState {
        db,
        debrid,
        notifier,
        is_scanning,
    });

    #[allow(unused_mut)]
    let mut app = Router::new()
        .merge(mimic_qbittorrent().with_state(state.clone()))
        .nest("/trpc", get_rpc_router().with_state(state));

    #[cfg(feature = "static")]
    {
        use tower_http::services::{ServeDir, ServeFile};

        let static_path = std::env::var("LUMIN_STATIC_PATH")
            .expect("LUMIN_STATIC_PATH not set with static feature");
        let index_path = static_path.clone() + "/index.html";
        let serve_dir = ServeDir::new(static_path)
            .not_found_service(ServeFile::new(index_path))
            .precompressed_gzip();

        app = app.fallback_service(serve_dir)
    }

    let bind_host = env::var("LUMIN_HOST").unwrap_or("127.0.0.1".to_string());
    let bind_port = env::var("LUMIN_PORT").unwrap_or("8000".to_string());
    let bind_addr = format!("{}:{}", bind_host, bind_port);
    let listener = TcpListener::bind(bind_addr).await.unwrap();
    info!("Listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(
            mount_handle,
            reconciler_handle,
            cache_handle,
            extractor_handle,
        ))
        .await
        .unwrap();

    Ok(())
}

async fn shutdown_signal(
    mount_handle: MountHandle,
    reconciler_handle: JoinHandle<()>,
    cache_handle: JoinHandle<()>,
    extractor_handle: JoinHandle<()>,
) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = reconciler_handle => {},
        _ = cache_handle => {},
        _ = extractor_handle => {},
        _ = ctrl_c => {
            info!("Received Ctrl+C");
        },
        _ = terminate => {
            info!("Received termination signal");
        },
    }

    debug!("Unmounting fs...");
    mount_handle.unmount().await.unwrap();
}
