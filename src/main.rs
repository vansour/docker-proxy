use axum::{
    extract::Request,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, head, post, put},
    Router,
};
use bytes::Bytes;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing::{debug, info};

mod config;
mod log;
mod proxy;
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
    init_logger(config.log_file_path(), &config.log_level_normalized())
        .or_else(|_| init_logger_console(&config.log_level_normalized()))
        .expect("Failed to initialize logger");

    info!("Docker Registry Proxy starting");
    info!("Configuration: {}", config.to_display_string());

    let proxy = Arc::new(DockerProxy::new());

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

// 日志中间件
async fn log_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    debug!("{} {}", method, uri);
    next.run(request).await
}

// 验证Docker Registry V2 API
async fn handle_v2_check() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Docker-Distribution-Api-Version",
        "registry/2.0".parse().unwrap(),
    );
    (StatusCode::OK, headers)
}

// 简单的根页面，返回一段 HTML 带镜像加速器工具
#[allow(dead_code)]
async fn root_index() -> impl IntoResponse {
    let _html = r#"
<!doctype html>
<html lang="zh-CN">
    <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width,initial-scale=1" />
        <title>docker-proxy - 镜像加速器</title>
        <style>
            body { font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial; padding: 2rem; }
            input, button { font-size: 1rem; padding: .5rem; }
            .box { max-width: 800px; margin: 0 auto; }
            pre { background:#f6f8fa; padding:1rem; border-radius:6px; overflow:auto }
            label { display:block; margin-top:1rem }
        </style>
    </head>
    <body>
        <div class="box">
            <h1>docker-proxy 镜像加速器</h1>
            <p>输入一个 Docker 镜像（可带 registry 前缀与 tag），生成通过本代理的加速链接与命令。</p>

            <label>镜像（例如 <code>ubuntu:latest</code> 或 <code>ghcr.io/vansour/gh-proxy:latest</code>）</label>
            <input id="image" placeholder="library/ubuntu:latest" style="width:100%" />
            <div style="margin-top:.75rem">
                <button id="btn">生成加速链接</button>
                <button id="copy" style="margin-left:.5rem">复制 docker pull</button>
            </div>

            <h3>结果</h3>
            <div id="result">
                <p>填写镜像并点击“生成加速链接”。</p>
            </div>

            <h3>说明</h3>
            <p>使用此加速域（示例）：<code>docker.gitvansour.top</code>。若为 HTTP，请将该域加入 Docker daemon 的 insecure-registries。</p>
        </div>

        <script>
            const host = location.hostname + (location.port ? ':'+location.port : '')
            const proxyHost = host; // 使用当前访问域作为代理域

            function parseImage(input) {
                // 支持 [registry/]path[:tag]
                let tag = 'latest'
                let img = input.trim()
                if (!img) return null
                const tagIdx = img.lastIndexOf(':')
                if (tagIdx > -1 && img.indexOf('/') < tagIdx) {
                    tag = img.slice(tagIdx+1)
                    img = img.slice(0, tagIdx)
                }
                return { name: img, tag }
            }

            function makeLinks(image) {
                // docker pull command
                const pull = `docker pull ${proxyHost}/${image.name}:${image.tag}`
                // manifest url
                const manifest = `http://${proxyHost}/v2/${image.name}/manifests/${image.tag}`
                // v2 probe
                const probe = `http://${proxyHost}/v2/`
                return { pull, manifest, probe }
            }

            document.getElementById('btn').addEventListener('click', ()=>{
                const v = document.getElementById('image').value || ''
                const parsed = parseImage(v)
                if (!parsed) { document.getElementById('result').innerHTML = '<p style="color:#b00">请输入镜像</p>'; return }
                const links = makeLinks(parsed)
                document.getElementById('result').innerHTML = `
                    <p><strong>docker pull:</strong></p>
                    <pre id="pullcmd">${links.pull}</pre>
                    <p><strong>manifest:</strong> <a href="${links.manifest}" target="_blank">${links.manifest}</a></p>
                    <p><strong>/v2/ probe:</strong> <a href="${links.probe}" target="_blank">${links.probe}</a></p>
                `
            })

            document.getElementById('copy').addEventListener('click', ()=>{
                const el = document.getElementById('pullcmd')
                if (!el) return
                navigator.clipboard.writeText(el.textContent).then(()=>{ alert('已复制到剪贴板') }).catch(()=>{ alert('复制失败，请手动复制') })
            })
        </script>
    </body>
</html>
"#;

    // legacy root_index kept for reference but not used (root now redirects to /web/)
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(""),
    )
}

// 健康检查：简单返回 JSON {"status":"ok"}
async fn healthz() -> impl IntoResponse {
    let body = r#"{"status":"ok"}"#;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
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
            headers.insert(
                header::CONTENT_TYPE,
                content_type
                    .parse()
                    .unwrap_or("application/json".parse().unwrap()),
            );
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("Error getting manifest: {}", e);
            (StatusCode::NOT_FOUND, format!("Error: {}", e)).into_response()
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
            headers.insert(
                header::CONTENT_TYPE,
                content_type
                    .parse()
                    .unwrap_or("application/json".parse().unwrap()),
            );
            headers.insert(
                header::CONTENT_LENGTH,
                content_length.to_string().parse().unwrap(),
            );
            (StatusCode::OK, headers).into_response()
        }
        Err(e) => {
            tracing::error!("Error heading manifest: {}", e);
            (StatusCode::NOT_FOUND, format!("Error: {}", e)).into_response()
        }
    }
}

// 获取 blob
async fn get_blob(
    State(proxy): State<Arc<DockerProxy>>,
    Path((name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    match proxy.get_blob(&name, &digest).await {
        Ok(body) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            headers.insert(
                header::CONTENT_LENGTH,
                body.len().to_string().parse().unwrap(),
            );
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("Error getting blob: {}", e);
            (StatusCode::NOT_FOUND, format!("Error: {}", e)).into_response()
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
            (StatusCode::NOT_FOUND, format!("Error: {}", e)).into_response()
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
            headers.insert(header::LOCATION, location.parse().unwrap());
            (StatusCode::ACCEPTED, headers).into_response()
        }
        Err(e) => {
            tracing::error!("Error initiating blob upload: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response()
        }
    }
}

// 完成 blob 上传
async fn complete_blob_upload() -> impl IntoResponse {
    (StatusCode::CREATED, "Upload complete")
}

// 简单静态文件服务：托管镜像位于镜像内的 `/app/web`，但不对外暴露 `/web/` 前缀路径
async fn serve_static(Path(file): Path<String>) -> impl IntoResponse {
    // sanitize and normalize
    let mut path = file.trim_start_matches('/').to_string();
    // explicitly disallow accessing the files under the `/web/` URL prefix
    if path == "web" || path.starts_with("web/") {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    if path.is_empty() || path.ends_with('/') {
        path = "index.html".to_string();
    }
    if path.contains("..") {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    let full = format!("/app/web/{}", path);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let content = Bytes::from(bytes);
            let ctype = if path.ends_with(".html") {
                "text/html; charset=utf-8"
            } else if path.ends_with(".js") {
                "application/javascript"
            } else if path.ends_with(".css") {
                "text/css"
            } else if path.ends_with(".svg") {
                "image/svg+xml"
            } else if path.ends_with(".png") {
                "image/png"
            } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
                "image/jpeg"
            } else {
                "application/octet-stream"
            };

            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, ctype.parse().unwrap());
            headers.insert(
                header::CONTENT_LENGTH,
                content.len().to_string().parse().unwrap(),
            );
            (StatusCode::OK, headers, content).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

// Serve the UI index at root (no redirect)
async fn serve_root() -> impl IntoResponse {
    let full = "/app/web/index.html".to_string();
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let content = Bytes::from(bytes);
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                "text/html; charset=utf-8".parse().unwrap(),
            );
            headers.insert(
                header::CONTENT_LENGTH,
                content.len().to_string().parse().unwrap(),
            );
            (StatusCode::OK, headers, content).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

// Wildcard dispatch handlers for /v2/*rest to support repository names containing '/'
async fn v2_get(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    // rest is the path after /v2/, e.g. "ghcr.io/vansour/gh-proxy/manifests/latest"
    let parts: Vec<&str> = rest.split('/').collect();
    // look for "manifests" segment
    if let Some(i) = parts.iter().position(|&p| p == "manifests") {
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let reference = parts[i + 1].to_string();
            return get_manifest(State(proxy), Path((name, reference))).await;
        }
    }

    // look for "blobs" segment (GET blob)
    if let Some(i) = parts.iter().position(|&p| p == "blobs") {
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let digest = parts[i + 1].to_string();
            let resp = get_blob(State(proxy), Path((name, digest))).await;
            return resp.into_response();
        }
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn v2_head(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    let parts: Vec<&str> = rest.split('/').collect();
    if let Some(i) = parts.iter().position(|&p| p == "manifests") {
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let reference = parts[i + 1].to_string();
            return head_manifest(State(proxy), Path((name, reference))).await;
        }
    }
    if let Some(i) = parts.iter().position(|&p| p == "blobs") {
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let digest = parts[i + 1].to_string();
            let resp = head_blob(State(proxy), Path((name, digest))).await;
            return resp.into_response();
        }
    }
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn v2_post(State(proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    let parts: Vec<&str> = rest.split('/').collect();
    // blobs uploads: .../blobs/uploads/
    if parts.len() >= 2 && parts.ends_with(&["blobs", "uploads"]) {
        let name = parts[..parts.len() - 2].join("/");
        let resp = initiate_blob_upload(State(proxy), Path(name)).await;
        return resp;
    }
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

async fn v2_put(State(_proxy): State<Arc<DockerProxy>>, Path(rest): Path<String>) -> Response {
    let parts: Vec<&str> = rest.split('/').collect();
    // complete upload: .../blobs/uploads/:uuid
    if parts.len() >= 3 && parts[parts.len() - 2] == "uploads" {
        // We intentionally don't process the uuid here; return created
        let resp = complete_blob_upload().await;
        return resp.into_response();
    }
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}
