use axum::{
    body::Body,
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use tokio_util::io::ReaderStream;

use crate::range;

/// 静态文件服务配置常量
pub mod static_file_config {
    /// 流式传输阈值：大于此值的文件将使用流式传输
    /// 1MB 是一个平衡点，既能减少小文件的开销，又能处理大文件
    pub const STREAM_THRESHOLD: u64 = 1024 * 1024;
}

/// 根据文件路径确定 Content-Type
pub fn get_content_type(path: &str) -> &'static str {
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
pub async fn serve_static(headers: HeaderMap, Path(file): Path<String>) -> impl IntoResponse {
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
    if let Some(ext) = canonical_path.extension()
        && let Some(ext_str) = ext.to_str()
        && !ALLOWED_EXTENSIONS.contains(&ext_str.to_lowercase().as_str())
    {
        tracing::warn!(
            "Blocked access to file with disallowed extension: {}",
            canonical_path.display()
        );
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    } else if canonical_path.extension().is_none() {
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

    use static_file_config::STREAM_THRESHOLD;

    if file_size < STREAM_THRESHOLD {
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
        match tokio::fs::File::open(&canonical_path).await {
            Ok(file) => {
                tracing::debug!(
                    file_path = %requested_path,
                    file_size_mb = file_size / (1024 * 1024),
                    "Serving large file via streaming"
                );
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
pub async fn serve_range(
    file_path: &std::path::Path,
    range: std::ops::Range<u64>,
    file_size: u64,
    content_type: &str,
    requested_path: &str,
) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let (status, headers) = match range::create_range_headers(&range, file_size, content_type) {
        Ok(result) => result,
        Err(_) => {
            tracing::error!("Failed to create range headers");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };

    let mut file = match tokio::fs::File::open(file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to open file for range request: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
        }
    };

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
pub async fn serve_root() -> impl IntoResponse {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_threshold() {
        use static_file_config::STREAM_THRESHOLD;

        assert_eq!(STREAM_THRESHOLD, 1024 * 1024);

        assert!(100 * 1024 < STREAM_THRESHOLD, "100KB should be in-memory");
        assert!(
            2 * 1024 * 1024 >= STREAM_THRESHOLD,
            "2MB should be streamed"
        );
    }

    #[test]
    fn test_content_type_mapping() {
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
}
