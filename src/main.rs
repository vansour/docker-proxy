use axum::{
    Router,
    extract::Request,
    middleware::{self, Next},
    response::Response,
    routing::{get, head, post, put},
};
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

mod api;
mod config;
mod error;
mod log;
mod proxy;
mod range;
mod router;
mod static_files;
use config::Config;
use log::{init_logger, init_logger_console};
use proxy::DockerProxy;
use static_files::{serve_root, serve_static};

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
        .route("/healthz", get(api::healthz))
        // 调试：查看 manifest size vs 实际 blob 大小
        .route("/debug/blob-info", get(api::debug_blob_info))
        // static web files served at root (handler below). API routes (/v2/*) are registered earlier.
        .route("/{*file}", get(serve_static))
        // serve web UI at root without redirect
        .route("/", get(serve_root))
        // Docker Registry V2 API endpoints
        .route("/v2/", get(api::handle_v2_check))
        // wildcard dispatch for repository names that may contain slashes (e.g. ghcr.io/owner/repo)
        .route("/v2/{*rest}", get(api::v2_get))
        .route("/v2/{*rest}", head(api::v2_head))
        .route("/v2/{*rest}", post(api::v2_post))
        .route("/v2/{*rest}", put(api::v2_put))
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

// api module declared above
