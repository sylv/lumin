use crate::mount::LuminFS;
use axum::Router;
use cache::Cache;
use config::get_config;
use debrid::Debrid;
use fuse3::raw::prelude::*;
use fuse3::{MountOptions, raw::MountHandle};
use qbittorrent::mimic_qbittorrent;
use reconciler::start_reconciler;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{
    env,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, signal, sync::Notify, task::JoinHandle, time::sleep};
use tracing::{error, info, warn};

mod cache;
mod config;
mod debrid;
mod error;
mod helpers;
mod mount;
mod qbittorrent;
mod reconciler;
mod state;

pub struct AppState {
    pub pool: SqlitePool,
    pub debrid: Arc<Debrid>,
    pub notifier: Arc<Notify>,
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

    let debrid = Arc::new(Debrid::new());
    let notifier = Arc::new(Notify::new());
    let config = get_config();

    let reconciler_handle = tokio::spawn({
        let pool = pool.clone();
        let debrid = debrid.clone();
        let notifier = notifier.clone();

        async move {
            run_with_retry("reconciler", || {
                let pool = pool.clone();
                let debrid = debrid.clone();
                let notifier = notifier.clone();

                async move {
                    start_reconciler(&pool, debrid, notifier)
                        .await
                        .map_err(|e| format!("Reconciler error: {}", e))
                }
            })
            .await
            .expect("Reconciler retry wrapper failed");
        }
    });

    let (cache, cache_handle) = {
        let debrid = debrid.clone();
        let pool = pool.clone();
        let cache = Cache::load(&pool, debrid).await.expect("Failed to load cache");

        let cache_handle = tokio::spawn({
            let cache = cache.clone();
            async move {
                run_with_retry("cache_sweeper", || {
                    let cache = cache.clone();
                    async move {
                        cache
                            .start_sweeper()
                            .await
                            .map_err(|e| format!("Cache sweeper error: {}", e))
                    }
                })
                .await
                .expect("Cache sweeper retry wrapper failed");
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
        let fs = LuminFS::new(pool.clone(), cache);
        let session = Session::new(mount_options);
        if config.mount_unprivileged {
            session.mount_with_unprivileged(fs, &config.mount_path).await.unwrap()
        } else {
            session.mount(fs, &config.mount_path).await.unwrap()
        }
    };

    let state = Arc::new(AppState { pool, debrid, notifier });
    let app = Router::new().merge(mimic_qbittorrent().with_state(state.clone()));

    let bind_host = env::var("LUMIN_HOST").unwrap_or("127.0.0.1".to_string());
    let bind_port = env::var("LUMIN_PORT").unwrap_or("8000".to_string());
    let bind_addr = format!("{}:{}", bind_host, bind_port);
    let listener = TcpListener::bind(bind_addr).await.unwrap();
    info!("Listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(mount_handle, reconciler_handle, cache_handle))
        .await
        .unwrap();

    Ok(())
}

async fn shutdown_signal(mount_handle: MountHandle, reconciler_handle: JoinHandle<()>, cache_handle: JoinHandle<()>) {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
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
        _ = ctrl_c => {
            info!("Received Ctrl+C");
        },
        _ = terminate => {
            info!("Received termination signal");
        },
    }

    drop(mount_handle);
}

/// Generic retry wrapper for background tasks
/// Retries failed tasks up to 3 times with 5-minute delays
/// Resets attempt counter if task runs for more than 60 seconds
/// Kills the program if all retries are exhausted
async fn run_with_retry<F, Fut>(task_name: &str, task_factory: F) -> Result<(), String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    const MAX_ATTEMPTS: u32 = 3;
    const RETRY_DELAY: Duration = Duration::from_secs(300);
    const RESET_THRESHOLD: Duration = Duration::from_secs(60);

    let mut attempts = 0;

    loop {
        attempts += 1;
        let start_time = Instant::now();

        info!("Starting {} (attempt {})", task_name, attempts);

        match task_factory().await {
            Ok(_) => {
                info!("{} completed successfully", task_name);
                return Ok(());
            }
            Err(e) => {
                let runtime = start_time.elapsed();

                if runtime >= RESET_THRESHOLD {
                    // Task ran for more than 60 seconds before failing, reset attempts
                    warn!(
                        "{} failed after running for {:?}, resetting attempt counter: {}",
                        task_name, runtime, e
                    );
                    attempts = 1; // Reset to 1 since we'll increment at start of next loop
                } else {
                    warn!("{} failed after {:?}: {}", task_name, runtime, e);
                }

                if attempts >= MAX_ATTEMPTS {
                    error!(
                        "{} failed {} times consecutively, killing program",
                        task_name, MAX_ATTEMPTS
                    );
                    std::process::exit(1);
                }

                warn!(
                    "{} will retry in {} seconds (attempt {} of {})",
                    task_name,
                    RETRY_DELAY.as_secs(),
                    attempts + 1,
                    MAX_ATTEMPTS
                );

                sleep(RETRY_DELAY).await;
            }
        }
    }
}
