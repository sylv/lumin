use crate::AppState;
use crate::config::get_config;
use crate::entities::torrents::TorrentState;
use crate::entities::{nodes, torrent_blocks, torrent_files, torrents};
use crate::error::AppError;
use crate::helpers::add_trackers_to_magnet_uri::add_trackers_to_magnet_uri;
use crate::helpers::parse_magnet_uri::parse_magnet_uri;
use axum::extract::{FromRequest, Multipart, Query, Request, State};
use axum::http::Method;
use axum::http::Uri;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Form, Json, Router};
use reqwest::StatusCode;
use rs_torrent_magnet::magnet_from_torrent;
use sea_orm::prelude::*;
use sea_orm::{Set, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

pub async fn auth_login() -> impl IntoResponse {
    "Ok."
}

async fn app_version() -> &'static str {
    "v4.3.2"
}

async fn app_webapi_version() -> &'static str {
    "2.7"
}

async fn app_buildinfo() -> impl IntoResponse {
    Json(json!({
        "bitness": 64,
        "boost": "1.75.0",
        "libtorrent": "1.2.11.0",
        "openssl": "1.1.1i",
        "qt": "5.15.2",
        "zlib": "1.2.11"
    }))
}

async fn app_shutdown() -> impl IntoResponse {
    StatusCode::OK
}

async fn app_preferences() -> impl IntoResponse {
    let config = get_config();
    let save_path = config
        .mount_path
        .join("downloads")
        .to_string_lossy()
        .into_owned();

    Json(json!({
        "save_path": save_path,
        "max_active_downloads": 5,
        "max_active_torrents": 10,
        "max_active_uploads": 5,
        "dht": true, // allows magnets with no trackers to be added
    }))
}

async fn app_set_preferences() -> impl IntoResponse {
    StatusCode::OK
}

async fn app_default_save_path() -> impl IntoResponse {
    let config = get_config();
    config
        .mount_path
        .join("downloads")
        .to_string_lossy()
        .into_owned()
}

#[derive(Debug, Deserialize)]
struct QBTorrentsInfoRequest {
    pub category: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QBittorrentTorrent {
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub progress: f32, // 0.0-1.0
    #[serde(rename = "eta")]
    pub eta_secs: u32,
    pub state: String,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub ratio: f32,
    pub ratio_limit: Option<f64>,
    pub seeding_time: Option<u32>,
    pub seeding_time_limit: Option<u32>,
    pub inactive_seeding_time_limit: Option<u32>,
    pub last_activity: u64,
}

async fn torrents_info(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QBTorrentsInfoRequest>,
) -> Result<impl IntoResponse, AppError> {
    let mut torrents =
        torrents::Entity::find().filter(torrents::Column::State.ne(TorrentState::Removing));

    if let Some(category) = query.category {
        torrents = torrents.filter(torrents::Column::Category.eq(category));
    }

    let torrents = torrents.all(&state.db).await?;

    Ok(Json(
        torrents
            .into_iter()
            .map(|v| v.to_qbittorrent())
            .collect::<Vec<_>>(),
    ))
}

#[derive(Debug, Deserialize)]
struct QBTorrentsHashRequest {
    pub hash: String,
}

async fn torrents_files(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QBTorrentsHashRequest>,
) -> Result<Response, AppError> {
    let Some(torrent) = torrents::Entity::find()
        .filter(torrents::Column::Hash.eq(&query.hash))
        .filter(torrents::Column::State.ne(TorrentState::Removing))
        .one(&state.db)
        .await?
    else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Torrent not found"})),
        )
            .into_response());
    };

    let files = torrent_files::Entity::find()
        .filter(torrent_files::Column::TorrentId.eq(torrent.id))
        .all(&state.db)
        .await?;

    Ok(Json(
        files
            .into_iter()
            .map(|f| {
                json!({
                    "name": f.path,
                    "size": f.size,
                    "progress": torrent.progress,
                    "priority": 1,
                    "piece_range": [0, 0],
                    "availability": 1.0,
                })
            })
            .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn torrent_properties(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QBTorrentsHashRequest>,
) -> Result<Response, AppError> {
    let Some(torrent) = torrents::Entity::find()
        .filter(torrents::Column::Hash.eq(&query.hash))
        .filter(torrents::Column::State.ne(TorrentState::Removing))
        .one(&state.db)
        .await?
    else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Torrent not found"})),
        )
            .into_response());
    };

    let torrent = torrent.to_qbittorrent();
    return Ok(Json(json!({
        "hash": torrent.hash,
        "save_path": torrent.save_path,
        "seeding_time": 0,
    }))
    .into_response());
}

#[derive(Debug, Deserialize)]
struct QBTorrentsDeleteRequest {
    pub hashes: String,
}

async fn torrents_delete(
    State(state): State<Arc<AppState>>,
    Form(request): Form<QBTorrentsDeleteRequest>,
) -> Result<Response, AppError> {
    let hashes: Vec<String> = request
        .hashes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if hashes.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No hashes provided"})),
        )
            .into_response());
    }

    let tx = state.db.begin().await?;

    for hash in &hashes {
        let Some(torrent) = torrents::Entity::find()
            .filter(torrents::Column::Hash.eq(hash))
            .one(&tx)
            .await?
        else {
            continue;
        };

        let references = nodes::Entity::find()
            .filter(nodes::Column::TorrentId.eq(torrent.id))
            .filter(nodes::Column::IsAutomatic.eq(false))
            .count(&tx)
            .await?;

        if references == 0 {
            // if the file has no nodes referencing it (aside from the download nodes),
            // we can mark it for removal.
            torrents::Entity::update(torrents::ActiveModel {
                id: Set(torrent.id),
                state: Set(TorrentState::Removing),
                ..Default::default()
            })
            .exec(&tx)
            .await?;
        } else {
            // however, if there are nodes outside downloads referencing it,
            // the torrent is still being used for *something* and we want to keep it around.
            // todo: it could be possible that eg, the episodes from the torrent are deleted but not the subtitle
            // files, in which case we should still allow the torrent to be removed.
            if torrent.category.is_some() {
                // todo: sonarr might try constantly remove the torrent because it will still show in the
                // torrents list. removing the category should help, assuming sonarr filters by its label,
                // but this also means labels are required to be configured with sonarr.
                torrents::Entity::update(torrents::ActiveModel {
                    id: Set(torrent.id),
                    category: Set(None),
                    ..Default::default()
                })
                .exec(&tx)
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok(StatusCode::OK.into_response())
}

async fn add_torrent(
    state: Arc<AppState>,
    magnet_uris: Vec<String>,
    category: Option<String>,
) -> Result<Response, AppError> {
    let tx = state.db.begin().await?;

    for magnet_uri in magnet_uris {
        let magnet_uri = add_trackers_to_magnet_uri(&magnet_uri);
        let Some(meta) = parse_magnet_uri(&magnet_uri) else {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid magnet URI"})),
            )
                .into_response());
        };

        let is_blocked = torrent_blocks::Entity::find()
            .filter(torrent_blocks::Column::Hash.eq(&meta.hash))
            .one(&tx)
            .await?;

        if let Some(block) = is_blocked {
            let reason = format!(
                "Torrent {} is blocked because of {}",
                meta.hash, block.block_reason
            );
            return Ok((StatusCode::FORBIDDEN, Json(json!({"error": reason}))).into_response());
        }

        let existing = torrents::Entity::find()
            .filter(torrents::Column::Hash.eq(&meta.hash))
            .one(&tx)
            .await?;

        if let Some(existing) = existing {
            // todo: ensure the files still exist on disk under the downloads dir
            if existing.category != category {
                // if the torrent already exists but has a different category, update it
                torrents::Entity::update(torrents::ActiveModel {
                    id: Set(existing.id),
                    category: Set(category),
                    ..Default::default()
                })
                .exec(&tx)
                .await?;
            }

            return Ok(StatusCode::OK.into_response());
        }

        torrents::Entity::insert(torrents::ActiveModel {
            hash: Set(meta.hash.clone()),
            name: Set(meta.name.unwrap_or(meta.hash)),
            category: Set(category.clone()),
            state: Set(TorrentState::Pending),
            magnet_uri: Set(magnet_uri),
            ..Default::default()
        })
        .exec(&tx)
        .await?;
    }

    tx.commit().await?;
    state.notifier.notify_one();
    Ok(StatusCode::OK.into_response())
}

#[derive(Debug, Deserialize)]
pub struct QBTorrentsAddRequest {
    pub urls: Option<String>,
    pub category: Option<String>,
}

async fn torrents_add_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QBTorrentsAddRequest>,
) -> Result<Response, AppError> {
    let urls = query.urls.as_deref().unwrap_or("");
    let magnet_uris: Vec<String> = urls
        .split(',')
        .filter_map(|s| s.trim().to_string().into())
        .collect();

    if magnet_uris.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No magnet URIs provided"})),
        )
            .into_response());
    }

    add_torrent(state, magnet_uris, query.category).await
}

async fn torrents_add_post(
    state: State<Arc<AppState>>,
    parts: Parts,
    req: Request,
) -> Result<Response, AppError> {
    // todo: unwrap heaven, this needs proper error handling
    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match content_type.split(";").next().unwrap_or("") {
        "application/x-www-form-urlencoded" => {
            let Form(data) = Form::<QBTorrentsAddRequest>::from_request(req, &state)
                .await
                .unwrap();

            let mut magnet_uris = Vec::new();
            if let Some(urls) = data.urls {
                for url in urls.split(',') {
                    if !url.is_empty() {
                        magnet_uris.push(url.to_string());
                    }
                }
            }

            add_torrent(state.0, magnet_uris, data.category).await
        }
        "multipart/form-data" => {
            let mut magnet_uris = Vec::new();
            let mut category = None;
            let mut multipart = Multipart::from_request(req, &state).await.unwrap();
            while let Some(field) = multipart.next_field().await.unwrap() {
                match field.name().unwrap() {
                    "category" => {
                        category = Some(field.text().await.unwrap());
                    }
                    "torrents" => {
                        // todo: a dependency for this is crazy, move this to a helper function
                        let bytes = field.bytes().await.unwrap();
                        let magnet_uri = magnet_from_torrent(bytes.to_vec()).unwrap();
                        magnet_uris.push(magnet_uri);
                    }
                    "urls" => {
                        let urls = field.text().await.unwrap();
                        for url in urls.split('\n') {
                            if !url.is_empty() {
                                magnet_uris.push(url.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }

            if magnet_uris.is_empty() {
                return Ok((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "No magnet URIs provided"})),
                )
                    .into_response());
            }

            add_torrent(state.0, magnet_uris, category).await
        }
        _ => {
            return Ok((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({"error": "Unsupported content type"})),
            )
                .into_response());
        }
    }
}

#[derive(Debug, Deserialize)]
struct QBTorrentsSetCategoryRequest {
    pub hashes: String,
    pub category: String,
}

async fn torrents_set_category(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QBTorrentsSetCategoryRequest>,
) -> Result<Response, AppError> {
    let hashes: Vec<String> = query
        .hashes
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    if hashes.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No hashes provided"})),
        )
            .into_response());
    }

    let updated = torrents::Entity::update_many()
        .filter(torrents::Column::Hash.is_in(hashes))
        .col_expr(
            torrents::Column::Category,
            Expr::value(query.category.clone()),
        )
        .exec(&state.db)
        .await?;

    if updated.rows_affected == 0 {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "No torrents found with the provided hashes"})),
        )
            .into_response());
    }

    Ok(StatusCode::OK.into_response())
}

async fn torrents_categories() -> Result<Response, AppError> {
    let config = get_config();
    let save_path = config
        .mount_path
        .join("downloads")
        .to_string_lossy()
        .into_owned();

    let mut category_map = HashMap::new();
    for category in &config.categories {
        category_map.insert(
            category.clone(),
            json!({
                "name": category,
                "savePath": save_path,
            }),
        );
    }

    Ok(Json(category_map).into_response())
}

#[derive(Debug, Deserialize)]
struct QBTorrentsCreateCategoryRequest {
    pub category: String,
}

async fn torrents_create_category(
    Form(request): Form<QBTorrentsCreateCategoryRequest>,
) -> impl IntoResponse {
    warn!(
        "Attempted to create a torrent category `{}`, you should properly configure your client or add the category manually.",
        request.category
    );

    (StatusCode::FORBIDDEN, "Torrent categories are hard coded.")
}

#[derive(Debug, Deserialize)]
struct QBTorrentsRemoveCategoryRequest {
    pub categories: String,
}

async fn torrents_remove_category(
    Form(request): Form<QBTorrentsRemoveCategoryRequest>,
) -> impl IntoResponse {
    warn!(
        "Attempted to remove torrent categories `{}`, you should properly configure your client or remove the category manually.",
        request.categories
    );
    (StatusCode::FORBIDDEN, "Torrent categories are hard coded.")
}

async fn fallback(uri: Uri, method: Method) -> impl IntoResponse {
    warn!("Missing implementation for route `{} {}`", method, uri);
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "Route not implemented"})),
    )
        .into_response()
}

pub fn mimic_qbittorrent() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v2/auth/login", get(auth_login).post(auth_login))
        .route("/api/v2/app/buildinfo", get(app_buildinfo))
        .route("/api/v2/app/shutdown", get(app_shutdown))
        .route(
            "/api/v2/app/preferences",
            get(app_preferences).post(app_set_preferences),
        )
        .route("/api/v2/app/defaultSavePath", get(app_default_save_path))
        .route("/api/v2/app/webapiVersion", get(app_webapi_version))
        .route("/api/v2/app/version", get(app_version))
        .route("/api/v2/torrents/info", get(torrents_info))
        .route("/api/v2/torrents/files", get(torrents_files))
        .route("/api/v2/torrents/properties", get(torrent_properties))
        .route(
            "/api/v2/torrents/delete",
            get(torrents_delete)
                .post(torrents_delete)
                .delete(torrents_delete),
        )
        .route(
            "/api/v2/torrents/add",
            get(torrents_add_get).post(torrents_add_post),
        )
        .route("/api/v2/torrents/setCategory", get(torrents_set_category))
        .route("/api/v2/torrents/categories", get(torrents_categories))
        .route(
            "/api/v2/torrents/createCategory",
            post(torrents_create_category),
        )
        .route(
            "/api/v2/torrents/removeCategory",
            post(torrents_remove_category),
        )
        .route("/api/v2/{*path}", any(fallback))
}
