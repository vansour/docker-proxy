use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{
    error,
    proxy::DockerProxy,
    router::{self, V2Endpoint},
};

// 验证Docker Registry V2 API
pub async fn handle_v2_check() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    if let Ok(value) = "registry/2.0".parse() {
        headers.insert("Docker-Distribution-Api-Version", value);
    } else {
        tracing::error!("Failed to parse Docker-Distribution-Api-Version header value");
    }
    (StatusCode::OK, headers)
}

// 健康检查：返回服务状态、版本信息和上游 registry 连通性
pub async fn healthz(State(proxy): State<Arc<DockerProxy>>) -> impl IntoResponse {
    use serde_json::json;

    const VERSION: &str = env!("CARGO_PKG_VERSION");

    let registry_healthy = proxy.check_registry_health().await;
    let registry_url = proxy.get_registry_url();

    let status = if registry_healthy {
        "healthy"
    } else {
        "degraded"
    };
    let http_status = if registry_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|e| {
            tracing::warn!("System time error, using 0: {}", e);
            std::time::Duration::from_secs(0)
        })
        .as_secs();

    let response = json!({
        "status": status,
        "version": VERSION,
        "registry": {
            "url": registry_url,
            "healthy": registry_healthy
        },
        "timestamp": timestamp
    });

    (
        http_status,
        [(header::CONTENT_TYPE, "application/json")],
        response.to_string(),
    )
}

// 调试接口：返回 manifest 中的 layer size 与实际 blob 大小
// 调用示例：
//   /debug/blob-info?name=library/debian&reference=latest&digest=sha256:...
pub async fn debug_blob_info(
    State(proxy): State<Arc<DockerProxy>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    use serde_json::json;

    let name = match params.get("name") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => {
            return (StatusCode::BAD_REQUEST, "missing 'name' query parameter").into_response();
        }
    };

    let digest = match params.get("digest") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => {
            return (StatusCode::BAD_REQUEST, "missing 'digest' query parameter").into_response();
        }
    };

    let reference = params
        .get("reference")
        .cloned()
        .unwrap_or_else(|| "latest".to_string());

    match proxy.debug_blob_info(&name, &digest, &reference).await {
        Ok((manifest_size, actual_size)) => {
            let body = json!({
                "name": name,
                "reference": reference,
                "digest": digest,
                "manifest_size": manifest_size,
                "actual_blob_size": actual_size,
                "size_diff": (actual_size as i64 - manifest_size as i64),
            })
            .to_string();

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("debug_blob_info error: {}", e);
            let status = match e {
                error::ProxyError::ManifestNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::BlobNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::BAD_GATEWAY,
            };
            (status, format!("debug error: {}", e)).into_response()
        }
    }
}

// 获取镜像manifest
async fn get_manifest(
    State(proxy): State<Arc<DockerProxy>>,
    Path((name, reference)): Path<(String, String)>,
) -> Response {
    match proxy.get_manifest(&name, &reference).await {
        Ok((content_type, body)) => {
            let mut headers = HeaderMap::new();
            let ct_value = content_type
                .parse()
                .or_else(|_| "application/json".parse())
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to parse content type '{}': {}", content_type, e);
                    HeaderValue::from_static("application/json")
                });
            headers.insert(header::CONTENT_TYPE, ct_value);
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("Error getting manifest: {}", e);
            let status = match e {
                error::ProxyError::ManifestNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, format!("Error: {}", e)).into_response()
        }
    }
}

// HEAD 请求 manifest
async fn head_manifest(
    State(proxy): State<Arc<DockerProxy>>,
    Path((name, reference)): Path<(String, String)>,
) -> Response {
    match proxy.head_manifest(&name, &reference).await {
        Ok((content_type, content_length)) => {
            let mut headers = HeaderMap::new();
            let ct_value = content_type
                .parse()
                .or_else(|_| "application/json".parse())
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to parse content type '{}': {}", content_type, e);
                    HeaderValue::from_static("application/json")
                });
            headers.insert(header::CONTENT_TYPE, ct_value);

            if let Ok(cl_value) = content_length.to_string().parse() {
                headers.insert(header::CONTENT_LENGTH, cl_value);
            } else {
                tracing::warn!("Failed to parse content length: {}", content_length);
            }
            (StatusCode::OK, headers).into_response()
        }
        Err(e) => {
            tracing::error!("Error heading manifest: {}", e);
            let status = match e {
                error::ProxyError::ManifestNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, format!("Error: {}", e)).into_response()
        }
    }
}

// 获取 blob：完全透传上游响应（包括头和流式 body）
async fn get_blob(
    State(proxy): State<Arc<DockerProxy>>,
    Path((name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    match proxy.get_blob(&name, &digest).await {
        Ok(upstream_resp) => {
            let status = axum::http::StatusCode::from_u16(upstream_resp.status().as_u16())
                .unwrap_or(StatusCode::OK);
            let mut headers = HeaderMap::new();

            for (key, value) in upstream_resp.headers().iter() {
                let key_str = key.as_str();
                if key_str.eq_ignore_ascii_case("connection")
                    || key_str.eq_ignore_ascii_case("transfer-encoding")
                    || key_str.eq_ignore_ascii_case("upgrade")
                {
                    continue;
                }

                if let Ok(ax_key) = axum::http::HeaderName::from_bytes(key_str.as_bytes())
                    && let Ok(ax_val) = axum::http::HeaderValue::from_bytes(value.as_bytes())
                {
                    headers.insert(ax_key, ax_val);
                }
            }

            let stream = upstream_resp.bytes_stream();
            let body = Body::from_stream(stream);

            (status, headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("Error getting blob: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Upstream blob error: {}", e),
            )
                .into_response()
        }
    }
}

// HEAD 请求 blob
async fn head_blob(
    State(proxy): State<Arc<DockerProxy>>,
    Path((name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    match proxy.head_blob(&name, &digest).await {
        Ok(content_length) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_LENGTH, content_length.to_string().as_str()),
            ],
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Error heading blob: {}", e);
            let status = match e {
                error::ProxyError::BlobNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, format!("Error: {}", e)).into_response()
        }
    }
}

// 初始化 blob 上传
async fn initiate_blob_upload(
    State(proxy): State<Arc<DockerProxy>>,
    Path(name): Path<String>,
) -> Response {
    match proxy.initiate_blob_upload(&name).await {
        Ok(upload_id) => {
            let mut headers = HeaderMap::new();
            let location = format!("/v2/{}/blobs/uploads/{}", name, upload_id);
            if let Ok(loc_value) = location.parse() {
                headers.insert(header::LOCATION, loc_value);
            } else {
                tracing::warn!("Failed to parse location header: {}", location);
            }
            (StatusCode::ACCEPTED, headers).into_response()
        }
        Err(e) => {
            tracing::error!("Error initiating blob upload: {}", e);
            let status = match e {
                error::ProxyError::BlobUploadNotSupported => StatusCode::METHOD_NOT_ALLOWED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, format!("Error: {}", e)).into_response()
        }
    }
}

// 完成 blob 上传
async fn complete_blob_upload() -> impl IntoResponse {
    (StatusCode::CREATED, "Upload complete")
}

// Wildcard dispatch handlers for /v2/*rest to support repository names containing '/'
pub async fn v2_get(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    match router::parse_v2_path(&rest) {
        V2Endpoint::Manifest { name, reference } => {
            get_manifest(State(proxy), Path((name, reference))).await
        }
        V2Endpoint::Blob { name, digest } => get_blob(State(proxy), Path((name, digest)))
            .await
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

pub async fn v2_head(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    match router::parse_v2_path(&rest) {
        V2Endpoint::Manifest { name, reference } => {
            head_manifest(State(proxy), Path((name, reference))).await
        }
        V2Endpoint::Blob { name, digest } => head_blob(State(proxy), Path((name, digest)))
            .await
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

pub async fn v2_post(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    match router::parse_v2_path(&rest) {
        V2Endpoint::BlobUploadInit { name } => initiate_blob_upload(State(proxy), Path(name)).await,
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

pub async fn v2_put(State(_proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    match router::parse_v2_path(&rest) {
        V2Endpoint::BlobUploadComplete { .. } => complete_blob_upload().await.into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}
