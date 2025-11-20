/// Range request support for HTTP partial content delivery
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use std::ops::Range;

/// Parse HTTP Range header
/// Example: "bytes=0-1023" or "bytes=1024-"
pub fn parse_range_header(range_header: &str, file_size: u64) -> Option<Range<u64>> {
    let range_header = range_header.trim();

    // Only support bytes range
    if !range_header.starts_with("bytes=") {
        return None;
    }

    let range_spec = &range_header[6..]; // Skip "bytes="
    let parts: Vec<&str> = range_spec.split('-').collect();

    if parts.len() != 2 {
        return None;
    }

    // Parse start and end
    let start = if parts[0].is_empty() {
        // Suffix range: "-500" means last 500 bytes
        if let Ok(suffix_length) = parts[1].parse::<u64>() {
            if suffix_length == 0 || suffix_length >= file_size {
                return Some(0..file_size);
            }
            return Some(file_size - suffix_length..file_size);
        }
        return None;
    } else {
        parts[0].parse::<u64>().ok()?
    };

    let end = if parts[1].is_empty() {
        // Open-ended range: "1024-" means from 1024 to end
        file_size
    } else {
        // Explicit end: "0-1023" means bytes 0 to 1023 inclusive
        // Add 1 to convert from inclusive end to exclusive end for Rust Range
        let end_inclusive = parts[1].parse::<u64>().ok()?;
        (end_inclusive + 1).min(file_size)
    };

    // Validate range
    if start >= file_size || start >= end {
        return None;
    }

    Some(start..end)
}

/// Create response headers for Range request
pub fn create_range_headers(
    range: &Range<u64>,
    file_size: u64,
    content_type: &str,
) -> Result<(StatusCode, HeaderMap), ()> {
    let mut headers = HeaderMap::new();

    // Content-Type
    if let Ok(ct_value) = content_type.parse::<HeaderValue>() {
        headers.insert(header::CONTENT_TYPE, ct_value);
    } else {
        return Err(());
    }

    // Content-Length (length of the range, not total file size)
    let content_length = range.end - range.start;
    if let Ok(cl_value) = content_length.to_string().parse::<HeaderValue>() {
        headers.insert(header::CONTENT_LENGTH, cl_value);
    } else {
        return Err(());
    }

    // Content-Range: bytes start-end/total
    let content_range = format!("bytes {}-{}/{}", range.start, range.end - 1, file_size);
    if let Ok(cr_value) = content_range.parse::<HeaderValue>() {
        headers.insert(header::CONTENT_RANGE, cr_value);
    } else {
        return Err(());
    }

    // Accept-Ranges
    if let Ok(ar_value) = "bytes".parse::<HeaderValue>() {
        headers.insert(header::ACCEPT_RANGES, ar_value);
    }

    Ok((StatusCode::PARTIAL_CONTENT, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_basic() {
        // Basic range: 0-1023
        let range = parse_range_header("bytes=0-1023", 10000);
        assert_eq!(range, Some(0..1024));

        // Open-ended range: 1024-
        let range = parse_range_header("bytes=1024-", 10000);
        assert_eq!(range, Some(1024..10000));

        // Suffix range: -500 (last 500 bytes)
        let range = parse_range_header("bytes=-500", 10000);
        assert_eq!(range, Some(9500..10000));
    }

    #[test]
    fn test_parse_range_edge_cases() {
        // Range exceeds file size
        let range = parse_range_header("bytes=0-20000", 10000);
        assert_eq!(range, Some(0..10000));

        // Start equals file size
        let range = parse_range_header("bytes=10000-", 10000);
        assert_eq!(range, None);

        // Start > end
        let range = parse_range_header("bytes=5000-1000", 10000);
        assert_eq!(range, None);

        // Invalid format
        let range = parse_range_header("bytes=abc-def", 10000);
        assert_eq!(range, None);

        // Not bytes range
        let range = parse_range_header("items=0-10", 10000);
        assert_eq!(range, None);
    }

    #[test]
    fn test_parse_range_small_file() {
        // Suffix larger than file
        let range = parse_range_header("bytes=-5000", 1000);
        assert_eq!(range, Some(0..1000));

        // Normal range on small file
        let range = parse_range_header("bytes=0-499", 1000);
        assert_eq!(range, Some(0..500));
    }

    #[test]
    fn test_create_range_headers() {
        let range = 1024..2048;
        let result = create_range_headers(&range, 10000, "application/octet-stream");

        assert!(result.is_ok());
        let (status, headers) = result.unwrap();

        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert!(headers.contains_key(header::CONTENT_TYPE));
        assert!(headers.contains_key(header::CONTENT_LENGTH));
        assert!(headers.contains_key(header::CONTENT_RANGE));
        assert!(headers.contains_key(header::ACCEPT_RANGES));

        // Verify Content-Length is range length, not total file size
        let content_length = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap();
        assert_eq!(content_length, 1024); // 2048 - 1024

        // Verify Content-Range format
        let content_range = headers
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert_eq!(content_range, "bytes 1024-2047/10000");
    }
}
