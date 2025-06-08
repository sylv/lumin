use crate::AppState;
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::{
    Router,
    extract::{Path, Query},
    response::IntoResponse,
    routing::get,
};
use regex::Regex;
use roxmltree::Document;
use std::collections::HashMap;
use std::sync::Arc;

// todo: cache cached states and only check for new ones
// todo: probably limit the debrid check to 1 at a time
async fn torznab_proxy(
    State(state): State<Arc<AppState>>,
    Path((upstream_url, path)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let upstream_url = urlencoding::decode(&upstream_url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid upstream URL: {}", e),
        )
    })?;

    let mut url = format!("{}/{}", upstream_url, path);
    if !params.is_empty() {
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        url = format!("{}?{}", url, query_string);
    }

    tracing::info!("Proxying request for `{}`", url);
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Failed to reach upstream: {}", e),
        )
    })?;

    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Failed to read response: {}", e),
        )
    })?;

    let func_type = params.get("t").cloned().unwrap_or_default();
    let modified_body = match func_type.as_str() {
        "search" | "tvsearch" | "movie" => match modify_torznab_response(state, &body).await {
            Ok(modified_body) => modified_body,
            Err(err) => {
                tracing::warn!(
                    "Failed to parse torznab response, passing through result: {}",
                    err
                );
                body
            }
        },
        "caps" => body,
        unknown => {
            tracing::warn!(
                "Unknown torznab function type: {}, passing through as-is",
                unknown
            );
            body
        }
    };

    let mut response_builder = axum::response::Response::builder().status(status);
    for (name, value) in headers.iter() {
        if name == "content-length" {
            continue;
        }

        response_builder = response_builder.header(name, value);
    }
    Ok(response_builder.body(modified_body).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to build response: {}", e),
        )
    })?)
}

fn get_infohash_from_item(item_xml: &str) -> Option<String> {
    let re = Regex::new(r#"<(?:torznab:)?attr\s+name="infohash"\s+value="([^"]+)""#).unwrap();
    re.captures(item_xml)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_lowercase()))
}

async fn modify_torznab_response(state: Arc<AppState>, xml_body: &str) -> anyhow::Result<String> {
    let doc = Document::parse(xml_body)?;

    let mut hashes = Vec::new();
    let mut item_positions = Vec::new();

    for node in doc.descendants() {
        if node.has_tag_name("item") {
            let item_xml = &xml_body[node.range()];

            if let Some(infohash) = get_infohash_from_item(item_xml) {
                hashes.push(infohash.clone());
                item_positions.push((node.range(), infohash));
            }
        }
    }

    if hashes.is_empty() {
        tracing::info!("No infohashes found in the response, returning original XML");
        return Ok(xml_body.to_string());
    }

    let mut cache_states = state.debrid.check_cached(&hashes).await?;
    tracing::info!("Adding cache states to {} items", cache_states.0.len());

    let mut modified_xml = xml_body.to_string();

    for (range, infohash) in item_positions.into_iter().rev() {
        if cache_states.0.remove(&infohash).is_some() {
            let item_xml_original_slice = &xml_body[range.clone()];
            let title_re = Regex::new(r"(<title>)(.*?)(</title>)").unwrap();
            if let Some(caps) = title_re.captures(item_xml_original_slice) {
                let title_content = caps.get(2).unwrap().as_str();
                let new_title_content = format!("[CACHED] {}", title_content);
                let new_item_xml_content = title_re.replace(
                    item_xml_original_slice,
                    format!("$1{}$3", new_title_content),
                );

                modified_xml.replace_range(range.start..range.end, &new_item_xml_content);
            }
        }
    }

    assert!(cache_states.0.is_empty(), "Not all cache states were used");
    Ok(modified_xml)
}

pub fn proxy_torznab() -> Router<Arc<AppState>> {
    Router::new().route("/torznab/{upstream_url}/{*path}", get(torznab_proxy))
}
