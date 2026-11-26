//! 开放接口的单元测试：来源校验、目录一致性、请求参数与错误分流。

use std::collections::HashMap;

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::json;

use super::catalog::{categories, endpoints};
use super::handlers::content_disposition;
use super::request::{legacy_stream, query_usize, separation_target, storage_error};
use super::router::{origin_allowed, parse_last_event_id};
use super::*;

#[test]
fn allows_local_and_webview_origins() {
    for origin in [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:5173",
        "http://127.0.0.1:8737",
        "https://127.0.0.1",
        "http://[::1]:5173",
        "http://LOCALHOST",
        "http://tauri.localhost:1420",
    ] {
        assert!(origin_allowed(origin), "{origin} 应被允许");
    }
}

#[test]
fn rejects_external_and_malformed_origins() {
    for origin in [
        "https://evil.example",
        "http://localhost.evil.example:5173",
        "http://127.0.0.1.evil.example",
        "http://192.168.1.10:8737",
        "null",
        "file://",
        "http://[::1",
        "ws://localhost:5173",
    ] {
        assert!(!origin_allowed(origin), "{origin} 应被拒绝");
    }
}

#[test]
fn frontend_codes_map_to_semantic_status() {
    let status = |code| CommandFailure::from_frontend(code, "x".to_owned()).status;
    assert_eq!(status(Some(400)), StatusCode::BAD_REQUEST);
    assert_eq!(status(Some(404)), StatusCode::NOT_FOUND);
    assert_eq!(status(Some(409)), StatusCode::CONFLICT);
    assert_eq!(status(Some(504)), StatusCode::GATEWAY_TIMEOUT);
    // 非错误状态或缺失时退回 400，避免把成功码当成错误返回。
    assert_eq!(status(Some(200)), StatusCode::BAD_REQUEST);
    assert_eq!(status(None), StatusCode::BAD_REQUEST);
}

#[test]
fn every_endpoint_belongs_to_a_declared_category() {
    let ids: Vec<&str> = categories().iter().map(|category| category.id).collect();
    assert_eq!(ids.len(), 9, "分类数量变化时同步更新测试");
    for endpoint in endpoints() {
        assert!(
            ids.contains(&endpoint.category),
            "{} {} 的分类 {} 未声明",
            endpoint.method,
            endpoint.path,
            endpoint.category
        );
    }
}

#[test]
fn every_category_has_endpoints() {
    let list = endpoints();
    for category in categories() {
        assert!(
            list.iter().any(|endpoint| endpoint.category == category.id),
            "分类 {} 没有接口",
            category.id
        );
    }
}

#[test]
fn endpoint_routes_are_unique_with_upper_methods() {
    let list = endpoints();
    let mut seen = std::collections::HashSet::new();
    for endpoint in &list {
        assert!(
            ["GET", "POST", "PUT", "DELETE"].contains(&endpoint.method),
            "非法方法：{}",
            endpoint.method
        );
        assert!(
            endpoint.path.starts_with("/v1/"),
            "路径必须以 /v1/ 开头：{}",
            endpoint.path
        );
        assert!(
            seen.insert((endpoint.method, endpoint.path)),
            "重复接口：{} {}",
            endpoint.method,
            endpoint.path
        );
    }
    // 清单与路由必须同步：数量变化时先确认 router 是否也加了对应路由。
    assert_eq!(list.len(), 33, "接口数量变化时同步更新测试");
}

#[test]
fn catalog_keeps_category_order() {
    let catalog = catalog();
    assert_eq!(catalog.categories.len(), 9);
    assert_eq!(catalog.categories[0].id, "system");
    assert_eq!(catalog.categories[4].id, "subtitles");
    assert_eq!(catalog.endpoints.len(), 33);
}

#[test]
fn catalog_serializes_for_the_settings_dialog() {
    let value = serde_json::to_value(catalog()).unwrap();
    assert!(value["categories"].is_array());
    assert_eq!(value["categories"][0]["id"], "system");
    assert_eq!(value["categories"][0]["label"], "系统与状态");
    assert!(value["endpoints"].is_array());
    let first = &value["endpoints"][0];
    assert_eq!(first["category"], "system");
    assert_eq!(first["method"], "GET");
    assert_eq!(first["path"], "/v1/health");
    assert!(first["summary"].is_string());
    assert!(first["description"].is_string());
}

#[test]
fn content_disposition_encodes_utf8_names() {
    let header = content_disposition("测试 曲目.srt");
    assert!(header.starts_with("attachment; filename=\""));
    assert!(header.contains("filename*=UTF-8''"));
    // 非 ASCII 全部百分号编码，中文标题不会破坏头部编码。
    assert!(header.contains("%E6%B5%8B%E8%AF%95"));
    assert!(!header.chars().any(|ch| ch as u32 > 127));
}

#[test]
fn parse_last_event_id_accepts_header_and_query() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("42"));
    assert_eq!(
        parse_last_event_id(&headers, &HashMap::new()).unwrap(),
        Some(42)
    );

    let query = HashMap::from([("lastEventId".to_owned(), "7".to_owned())]);
    assert_eq!(
        parse_last_event_id(&HeaderMap::new(), &query).unwrap(),
        Some(7)
    );

    let bad = HashMap::from([("lastEventId".to_owned(), "abc".to_owned())]);
    assert!(parse_last_event_id(&HeaderMap::new(), &bad).is_err());
}

#[test]
fn separation_target_only_accepts_known_values() {
    assert_eq!(separation_target(None).unwrap(), None);
    assert_eq!(
        separation_target(Some(&json!("vocals"))).unwrap(),
        Some("vocals".to_owned())
    );
    assert!(separation_target(Some(&json!("drums"))).is_err());
    assert_eq!(
        separation_target(Some(&json!("  "))).unwrap(),
        None,
        "空白视为未指定"
    );
}

#[test]
fn query_usize_rejects_garbage_and_clamps_range() {
    let empty = HashMap::new();
    assert_eq!(query_usize(&empty, "limit", 50, 1, 200).unwrap(), 50);

    let over = HashMap::from([("limit".to_owned(), "500".to_owned())]);
    assert_eq!(query_usize(&over, "limit", 50, 1, 200).unwrap(), 200);

    let bad = HashMap::from([("limit".to_owned(), "abc".to_owned())]);
    let error = query_usize(&bad, "limit", 50, 1, 200).unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[test]
fn storage_errors_split_io_and_validation() {
    let io = storage_error(anyhow::Error::new(std::io::Error::other("磁盘已满")));
    assert_eq!(io.status, StatusCode::INTERNAL_SERVER_ERROR);

    let validation = storage_error(anyhow::anyhow!("分段为空"));
    assert_eq!(validation.status, StatusCode::BAD_REQUEST);

    let busy = storage_error(anyhow::anyhow!(crate::key_lock::BUSY_MESSAGE));
    assert_eq!(busy.status, StatusCode::CONFLICT);
}

#[test]
fn legacy_flag_accepts_common_truthy_values() {
    for value in ["1", "true", "YES"] {
        let query = HashMap::from([("legacy".to_owned(), value.to_owned())]);
        assert!(legacy_stream(&query), "{value} 应视为 legacy");
    }
    assert!(!legacy_stream(&HashMap::new()));
    let off = HashMap::from([("legacy".to_owned(), "0".to_owned())]);
    assert!(!legacy_stream(&off));
}
