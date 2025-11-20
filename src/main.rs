use axum::{
    body::Body,
    extract::Request,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, head, post, put},
    Router,
};
use bytes::Bytes;
use std::sync::Arc;
use tokio_util::io::ReaderStream;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

mod config;
mod error;
mod log;
mod proxy;
mod range;
mod router;

use config::Config;
use log::{init_logger, init_logger_console};
use proxy::DockerProxy;

#[tokio::main]
async fn main() {
    // Load configuration
    let config = Config::from_file("/config/config.toml")
        .or_else(|_| Config::from_file("./config/config.toml"))
        .expect("Failed to load configuration");

    // Initialize logger based on configuration
    let _guard = init_logger(config.log_file_path(), &config.log_level_normalized())
        .or_else(|_| init_logger_console(&config.log_level_normalized()))
        .expect("Failed to initialize logger");

    info!("Docker Registry Proxy starting");
    info!("Configuration: {}", config.to_display_string());

    let proxy = Arc::new(DockerProxy::new(&config));

    // 构建路由
    let app = Router::new()
        // health check endpoint
        .route("/healthz", get(healthz))
        // static web files served at root (handler below). API routes (/v2/*) are registered earlier.
        .route("/*file", get(serve_static))
        // serve web UI at root without redirect
        .route("/", get(serve_root))
        // Docker Registry V2 API endpoints
        .route("/v2/", get(handle_v2_check))
        // wildcard dispatch for repository names that may contain slashes (e.g. ghcr.io/owner/repo)
        .route("/v2/*rest", get(v2_get))
        .route("/v2/*rest", head(v2_head))
        .route("/v2/*rest", post(v2_post))
        .route("/v2/*rest", put(v2_put))
        .layer(middleware::from_fn(log_middleware))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(proxy);

    let listener = tokio::net::TcpListener::bind(config.server_addr())
        .await
        .expect("Failed to bind to address");

    info!(
        "Docker Registry Proxy listening on http://{}",
        config.server_addr()
    );

    axum::serve(listener, app).await.expect("Server error");
}

// 日志中间件：记录请求、响应状态码和耗时（结构化日志）
async fn log_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let request_id = uuid::Uuid::new_v4();
    let start = std::time::Instant::now();

    // 获取客户端 IP（从 X-Forwarded-For 或连接地址）
    let client_ip = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // 处理请求
    let response = next.run(request).await;

    // 计算耗时
    let elapsed = start.elapsed();
    let status = response.status();
    let duration_ms = elapsed.as_secs_f64() * 1000.0;

    // 根据状态码选择日志级别，使用结构化字段
    if status.is_server_error() {
        tracing::error!(
            request_id = %request_id,
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            duration_ms = format!("{:.2}", duration_ms),
            client_ip = %client_ip,
            "Request completed with server error"
        );
    } else if status.is_client_error() {
        tracing::warn!(
            request_id = %request_id,
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            duration_ms = format!("{:.2}", duration_ms),
            client_ip = %client_ip,
            "Request completed with client error"
        );
    } else {
        tracing::info!(
            request_id = %request_id,
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            duration_ms = format!("{:.2}", duration_ms),
            client_ip = %client_ip,
            "Request completed successfully"
        );
    }

    response
}

// 验证Docker Registry V2 API
async fn handle_v2_check() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    if let Ok(value) = "registry/2.0".parse() {
        headers.insert("Docker-Distribution-Api-Version", value);
    } else {
        tracing::error!("Failed to parse Docker-Distribution-Api-Version header value");
    }
    (StatusCode::OK, headers)
}

// 健康检查：返回服务状态、版本信息和上游 registry 连通性
async fn healthz(State(proxy): State<Arc<DockerProxy>>) -> impl IntoResponse {
    use serde_json::json;

    // 获取版本信息（从环境变量或编译时信息）
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    // 检查上游 registry 连通性
    let registry_healthy = proxy.check_registry_health().await;
    let registry_url = proxy.get_registry_url();

    // 确定整体健康状态
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

    // 构建响应 JSON
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
            // 将 reqwest::Response 拆成头和 body 流，并适配到 axum 类型
            let status = axum::http::StatusCode::from_u16(upstream_resp.status().as_u16())
                .unwrap_or(StatusCode::OK);
            let mut headers = HeaderMap::new();

            for (key, value) in upstream_resp.headers().iter() {
                let key_str = key.as_str();
                // 过滤掉 hop-by-hop 头
                if key_str.eq_ignore_ascii_case("connection")
                    || key_str.eq_ignore_ascii_case("transfer-encoding")
                    || key_str.eq_ignore_ascii_case("upgrade")
                {
                    continue;
                }

                if let Ok(ax_key) = axum::http::HeaderName::from_bytes(key_str.as_bytes()) {
                    if let Ok(ax_val) = axum::http::HeaderValue::from_bytes(value.as_bytes()) {
                        headers.insert(ax_key, ax_val);
                    }
                }
            }

            let stream = upstream_resp.bytes_stream();
            let body = Body::from_stream(stream);

            (status, headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("Error getting blob: {}", e);
            let status = match e {
                error::ProxyError::BlobNotFound { .. } => StatusCode::NOT_FOUND,
                error::ProxyError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, format!("Error: {}", e)).into_response()
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

/// 静态文件服务配置常量
mod static_file_config {
    /// 流式传输阈值：大于此值的文件将使用流式传输
    /// 1MB 是一个平衡点，既能减少小文件的开销，又能处理大文件
    pub const STREAM_THRESHOLD: u64 = 1024 * 1024;
}

/// 根据文件路径确定 Content-Type
fn get_content_type(path: &str) -> &'static str {
    if path.ends_with(".html") || path.ends_with(".htm") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".woff") {
        "font/woff"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".ttf") {
        "font/ttf"
    } else if path.ends_with(".eot") {
        "application/vnd.ms-fontobject"
    } else {
        "application/octet-stream"
    }
}

// 安全的静态文件服务：使用 canonicalize 和白名单防止路径穿越，支持流式传输和 Range 请求
async fn serve_static(headers: HeaderMap, Path(file): Path<String>) -> impl IntoResponse {
    use std::path::PathBuf;

    // 白名单：只允许这些文件扩展名
    const ALLOWED_EXTENSIONS: &[&str] = &[
        "html", "htm", "css", "js", "json", "svg", "png", "jpg", "jpeg", "gif", "webp", "ico",
        "woff", "woff2", "ttf", "eot",
    ];

    // 基础目录
    let base_dir = PathBuf::from("/app/web");

    // 清理和规范化路径
    let mut requested_path = file.trim_start_matches('/').to_string();

    // 明确禁止访问 /web/ 前缀
    if requested_path == "web" || requested_path.starts_with("web/") {
        tracing::warn!("Blocked access to restricted path: {}", requested_path);
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    // 处理空路径或目录
    if requested_path.is_empty() || requested_path.ends_with('/') {
        requested_path = "index.html".to_string();
    }

    // 快速检查：拒绝包含 ".." 的路径
    if requested_path.contains("..") {
        tracing::warn!("Blocked path traversal attempt: {}", requested_path);
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    // 构造完整路径
    let full_path = base_dir.join(&requested_path);

    // 使用 canonicalize 防止路径穿越攻击
    let canonical_path = match tokio::fs::canonicalize(&full_path).await {
        Ok(path) => path,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }
    };

    // 确保规范化后的路径仍在基础目录内
    if !canonical_path.starts_with(&base_dir) {
        tracing::warn!(
            "Blocked access outside base directory: {}",
            canonical_path.display()
        );
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    // 检查文件扩展名白名单
    if let Some(ext) = canonical_path.extension() {
        if let Some(ext_str) = ext.to_str() {
            if !ALLOWED_EXTENSIONS.contains(&ext_str.to_lowercase().as_str()) {
                tracing::warn!(
                    "Blocked access to file with disallowed extension: {}",
                    canonical_path.display()
                );
                return (StatusCode::FORBIDDEN, "Forbidden").into_response();
            }
        }
    } else {
        // 没有扩展名的文件也被拒绝（除非是 index.html 等）
        tracing::warn!(
            "Blocked access to file without extension: {}",
            canonical_path.display()
        );
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    // 获取文件元数据以确定文件大小
    let metadata = match tokio::fs::metadata(&canonical_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("File not found or metadata error: {}", e);
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }
    };

    let file_size = metadata.len();

    // 根据文件扩展名确定 Content-Type
    let ctype = get_content_type(&requested_path);

    // 检查是否是 Range 请求
    let range_request = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| range::parse_range_header(s, file_size));

    // 如果是 Range 请求，返回部分内容
    if let Some(range) = range_request {
        return serve_range(&canonical_path, range, file_size, ctype, &requested_path).await;
    }

    // 构建响应头（完整文件）
    let mut response_headers = HeaderMap::new();
    if let Ok(ct_value) = ctype.parse() {
        response_headers.insert(header::CONTENT_TYPE, ct_value);
    } else {
        tracing::warn!("Failed to parse content type '{}', using default", ctype);
        response_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }

    if let Ok(cl_value) = file_size.to_string().parse() {
        response_headers.insert(header::CONTENT_LENGTH, cl_value);
    } else {
        tracing::warn!("Failed to parse content length: {}", file_size);
    }

    if let Ok(nosniff_value) = "nosniff".parse() {
        response_headers.insert("X-Content-Type-Options", nosniff_value);
    } else {
        tracing::warn!("Failed to parse X-Content-Type-Options header");
    }

    // 添加 Accept-Ranges header 表示支持 Range 请求
    if let Ok(ar_value) = "bytes".parse() {
        response_headers.insert(header::ACCEPT_RANGES, ar_value);
    }

    // 性能优化：根据文件大小选择不同的传输策略
    // - 小文件（< 1MB）：直接读取到内存，减少系统调用开销
    // - 大文件（>= 1MB）：使用流式传输，节省内存，支持大文件传输
    use static_file_config::STREAM_THRESHOLD;

    if file_size < STREAM_THRESHOLD {
        // 小文件策略：一次性读取到内存
        // 优点：速度快，延迟低
        // 适用：HTML、CSS、JS、小图片等
        match tokio::fs::read(&canonical_path).await {
            Ok(bytes) => {
                tracing::debug!(
                    file_path = %requested_path,
                    file_size_kb = file_size / 1024,
                    "Serving small file from memory"
                );
                let content = Bytes::from(bytes);
                (StatusCode::OK, response_headers, content).into_response()
            }
            Err(e) => {
                tracing::error!("File read error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
            }
        }
    } else {
        // 大文件策略：流式传输
        // 优点：内存占用低，支持任意大小文件
        // 适用：大图片、字体文件、视频等
        match tokio::fs::File::open(&canonical_path).await {
            Ok(file) => {
                tracing::debug!(
                    file_path = %requested_path,
                    file_size_mb = file_size / (1024 * 1024),
                    "Serving large file via streaming"
                );
                // 创建异步流式读取器，按需读取文件内容
                let stream = ReaderStream::new(file);
                let body = Body::from_stream(stream);
                (StatusCode::OK, response_headers, body).into_response()
            }
            Err(e) => {
                tracing::error!("File open error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
            }
        }
    }
}

// 处理 Range 请求，返回部分内容
async fn serve_range(
    file_path: &std::path::Path,
    range: std::ops::Range<u64>,
    file_size: u64,
    content_type: &str,
    requested_path: &str,
) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    // 创建 Range 响应头
    let (status, headers) = match range::create_range_headers(&range, file_size, content_type) {
        Ok(result) => result,
        Err(_) => {
            tracing::error!("Failed to create range headers");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };

    // 打开文件并定位到 range 起始位置
    let mut file = match tokio::fs::File::open(file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to open file for range request: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };

    // Seek 到起始位置
    if let Err(e) = file.seek(std::io::SeekFrom::Start(range.start)).await {
        tracing::error!("Failed to seek file: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    }

    let range_length = range.end - range.start;

    tracing::debug!(
        file_path = %requested_path,
        range_start = range.start,
        range_end = range.end,
        range_length = range_length,
        "Serving range request"
    );

    // 读取指定范围的数据
    let mut buffer = vec![0u8; range_length as usize];
    match file.read_exact(&mut buffer).await {
        Ok(_) => {
            let content = Bytes::from(buffer);
            (status, headers, content).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to read range from file: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

// Serve the UI index at root (no redirect)
async fn serve_root() -> impl IntoResponse {
    let full = "/app/web/index.html".to_string();
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let content = Bytes::from(bytes);
            let mut headers = HeaderMap::new();
            if let Ok(ct_value) = "text/html; charset=utf-8".parse() {
                headers.insert(header::CONTENT_TYPE, ct_value);
            } else {
                tracing::error!("Failed to parse HTML content type header");
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            }

            if let Ok(cl_value) = content.len().to_string().parse() {
                headers.insert(header::CONTENT_LENGTH, cl_value);
            } else {
                tracing::warn!("Failed to parse content length: {}", content.len());
            }
            (StatusCode::OK, headers, content).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

// Wildcard dispatch handlers for /v2/*rest to support repository names containing '/'
async fn v2_get(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    use router::{parse_v2_path, V2Endpoint};

    match parse_v2_path(&rest) {
        V2Endpoint::Manifest { name, reference } => {
            get_manifest(State(proxy), Path((name, reference))).await
        }
        V2Endpoint::Blob { name, digest } => get_blob(State(proxy), Path((name, digest)))
            .await
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

async fn v2_head(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    use router::{parse_v2_path, V2Endpoint};

    match parse_v2_path(&rest) {
        V2Endpoint::Manifest { name, reference } => {
            head_manifest(State(proxy), Path((name, reference))).await
        }
        V2Endpoint::Blob { name, digest } => head_blob(State(proxy), Path((name, digest)))
            .await
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

async fn v2_post(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    use router::{parse_v2_path, V2Endpoint};

    match parse_v2_path(&rest) {
        V2Endpoint::BlobUploadInit { name } => initiate_blob_upload(State(proxy), Path(name)).await,
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

async fn v2_put(State(_proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    use router::{parse_v2_path, V2Endpoint};

    match parse_v2_path(&rest) {
        V2Endpoint::BlobUploadComplete { .. } => complete_blob_upload().await.into_response(),
        _ => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_threshold() {
        use static_file_config::STREAM_THRESHOLD;

        // Verify threshold is 1MB
        assert_eq!(STREAM_THRESHOLD, 1024 * 1024);

        // Test file size categorization
        assert!(100 * 1024 < STREAM_THRESHOLD, "100KB should be in-memory");
        assert!(
            2 * 1024 * 1024 >= STREAM_THRESHOLD,
            "2MB should be streamed"
        );
    }

    #[test]
    fn test_content_type_mapping() {
        // Test common file types
        assert_eq!(get_content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(get_content_type("style.css"), "text/css; charset=utf-8");
        assert_eq!(
            get_content_type("script.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            get_content_type("data.json"),
            "application/json; charset=utf-8"
        );
        assert_eq!(get_content_type("logo.svg"), "image/svg+xml");
        assert_eq!(get_content_type("image.png"), "image/png");
        assert_eq!(get_content_type("photo.jpg"), "image/jpeg");
        assert_eq!(get_content_type("photo.jpeg"), "image/jpeg");
        assert_eq!(get_content_type("icon.gif"), "image/gif");
        assert_eq!(get_content_type("image.webp"), "image/webp");
        assert_eq!(get_content_type("favicon.ico"), "image/x-icon");
        assert_eq!(get_content_type("font.woff"), "font/woff");
        assert_eq!(get_content_type("font.woff2"), "font/woff2");
        assert_eq!(get_content_type("font.ttf"), "font/ttf");
        assert_eq!(
            get_content_type("font.eot"),
            "application/vnd.ms-fontobject"
        );
        assert_eq!(get_content_type("unknown.xyz"), "application/octet-stream");
    }

    #[test]
    fn test_file_size_categories() {
        use static_file_config::STREAM_THRESHOLD;

        // Typical web asset sizes
        let small_file = 50 * 1024; // 50KB
        let medium_file = 500 * 1024; // 500KB
        let large_file = 5 * 1024 * 1024; // 5MB

        assert!(small_file < STREAM_THRESHOLD);
        assert!(medium_file < STREAM_THRESHOLD);
        assert!(large_file >= STREAM_THRESHOLD);
    }

    #[test]
    fn test_version_constant() {
        // Verify version is defined and not empty
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        assert!(!VERSION.is_empty(), "Version should not be empty");
        assert!(
            VERSION.chars().any(|c| c.is_numeric()),
            "Version should contain numbers"
        );
    }
}
