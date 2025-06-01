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
use edit_xml::{Document, Element};
use std::collections::HashMap;
use std::sync::Arc;

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

    // Check if this is a search query (contains 't' parameter for search)
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

fn get_infohash(item: &Element, doc: &Document) -> Option<String> {
    let attrs = item
        .child_elements(&doc)
        .into_iter()
        .filter(|el| el.name(&doc) == "torznab:attr" || el.name(&doc) == "attr")
        .collect::<Vec<_>>();

    for attr in attrs {
        if let Some(name) = attr.attributes(&doc).get("name") {
            if name == "infohash" {
                return attr.attributes(&doc).get("value").map(|v| v.to_lowercase());
            }
        }
    }
    None
}

async fn modify_torznab_response(state: Arc<AppState>, xml_body: &str) -> anyhow::Result<String> {
    let mut doc = Document::parse_str(xml_body)?;
    let rss_el = doc.root_element().unwrap();
    let channel_el = rss_el
        .child_elements(&doc)
        .into_iter()
        .find(|el| el.name(&doc) == "channel")
        .ok_or_else(|| anyhow::anyhow!("No <channel> element found in the XML"))?;

    let items = channel_el
        .child_elements(&doc)
        .into_iter()
        .filter(|el| el.name(&doc) == "item")
        .collect::<Vec<_>>();

    let hashes = items
        .iter()
        .filter_map(|item| get_infohash(item, &doc))
        .collect::<Vec<_>>();

    let mut cache_states = state.debrid.check_cached(&hashes).await?;
    tracing::info!("Adding cache states to {} items", cache_states.0.len());

    for item in items {
        let Some(infohash) = get_infohash(&item, &doc) else {
            continue;
        };

        if cache_states.0.remove(&infohash).is_some() {
            // add [CACHED] to the front of the name
            let title_el = item
                .child_elements(&doc)
                .into_iter()
                .find(|el| el.name(&doc) == "title")
                .ok_or_else(|| anyhow::anyhow!("No <title> element found in the item"))?;

            let title = title_el.text_content(&doc);
            let new_title = format!("[CACHED] {}", title);
            title_el.set_text_content(&mut doc, new_title);
        }
    }

    assert!(cache_states.0.is_empty(), "Not all cache states were used");
    Ok(doc.write_str()?)
}

pub fn proxy_torznab() -> Router<Arc<AppState>> {
    Router::new().route("/torznab/{upstream_url}/{*path}", get(torznab_proxy))
}
