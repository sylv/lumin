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
use std::path::PathBuf;
use std::{
    env,
    str::FromStr,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, signal, sync::Notify, task::JoinHandle, time::sleep};
use tracing::{debug, error, info, warn};

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

async fn try_unmount(mount_path: &PathBuf) {
    let config = get_config();
    if !config.ensure_unmounted {
        return;
    }

    let mount_path_str = mount_path.to_string_lossy();
    let result = tokio::process::Command::new("fusermount3")
        .arg("-u")
        .arg(&*mount_path_str)
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            info!("Successfully unmounted {}", mount_path_str);
            return;
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not mounted") {
                // If fusermount says it's not mounted, we can ignore this
                info!("{} is not mounted", mount_path_str);
                return;
            }

            warn!(
                "Failed to unmount {}, this may cause issues: {}",
                mount_path_str, stderr
            );
        }
        Err(e) => {
            warn!("Failed to execute fusermount3: {}", e);
        }
    }
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
            run_with_retry("reconciler", || {
                let db = db.clone();
                let debrid = debrid.clone();
                let notifier = notifier.clone();
                let is_scanning = is_scanning.clone();

                async move {
                    start_reconciler(&db, debrid, notifier, is_scanning)
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
        let db = db.clone();
        let cache = Cache::load(&db, debrid)
            .await
            .expect("Failed to load cache");

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

        // todo: this might not be "proper", but if the process panics or in some other niche scenarios,
        // the mount can be left intact and on restart it will fail to mount.
        // this tries to avoid that by attempting to unmount the mount path before mounting again.
        // this will probably fail in a lot of scenarios and may do bad things, but it will fix infinite boot loops
        // under docker when the process shits its pants and restarts.
        try_unmount(&config.mount_path).await;

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
            run_with_retry("duration_extractor", || {
                let db = db.clone();
                let cache = cache.clone();
                async move {
                    start_duration_extractor(db, cache)
                        .await
                        .map_err(|e| format!("Duration extractor error: {}", e))
                }
            })
            .await
            .expect("Duration extractor retry wrapper failed");
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
