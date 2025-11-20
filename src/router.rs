/// Docker Registry V2 API endpoint types
#[derive(Debug, PartialEq)]
pub enum V2Endpoint {
    /// GET/HEAD manifest: /v2/{name}/manifests/{reference}
    Manifest { name: String, reference: String },
    /// GET/HEAD blob: /v2/{name}/blobs/{digest}
    Blob { name: String, digest: String },
    /// POST blob upload: /v2/{name}/blobs/uploads/
    BlobUploadInit { name: String },
    /// PUT blob upload: /v2/{name}/blobs/uploads/{uuid}
    BlobUploadComplete { name: String, uuid: String },
    /// Unknown or unsupported endpoint
    Unknown,
}

/// Parse Docker Registry V2 API path
///
/// # Arguments
/// * `rest` - The path after /v2/, e.g. "library/ubuntu/manifests/latest"
///
/// # Returns
/// The parsed endpoint type with extracted parameters
pub fn parse_v2_path(rest: &str) -> V2Endpoint {
    let parts: Vec<&str> = rest.split('/').collect();

    // Check for manifests endpoint: .../manifests/{reference}
    if let Some(i) = parts.iter().position(|&p| p == "manifests") {
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let reference = parts[i + 1].to_string();
            return V2Endpoint::Manifest { name, reference };
        }
    }

    // Check for blobs endpoint: .../blobs/{digest}
    if let Some(i) = parts.iter().position(|&p| p == "blobs") {
        // Blob upload complete: .../blobs/uploads/{uuid}
        if i + 2 < parts.len() && parts[i + 1] == "uploads" {
            let name = parts[..i].join("/");
            let uuid = parts[i + 2].to_string();
            return V2Endpoint::BlobUploadComplete { name, uuid };
        }
        // Blob upload init: .../blobs/uploads/
        if i + 1 < parts.len() && parts[i + 1] == "uploads" && i + 2 == parts.len() {
            let name = parts[..i].join("/");
            return V2Endpoint::BlobUploadInit { name };
        }
        // Regular blob access: .../blobs/{digest}
        if i + 1 < parts.len() {
            let name = parts[..i].join("/");
            let digest = parts[i + 1].to_string();
            return V2Endpoint::Blob { name, digest };
        }
    }

    V2Endpoint::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest_endpoint() {
        let endpoint = parse_v2_path("library/ubuntu/manifests/latest");
        assert_eq!(
            endpoint,
            V2Endpoint::Manifest {
                name: "library/ubuntu".to_string(),
                reference: "latest".to_string()
            }
        );

        // Test with nested repository names
        let endpoint = parse_v2_path("ghcr.io/vansour/docker-proxy/manifests/v1.0.0");
        assert_eq!(
            endpoint,
            V2Endpoint::Manifest {
                name: "ghcr.io/vansour/docker-proxy".to_string(),
                reference: "v1.0.0".to_string()
            }
        );
    }

    #[test]
    fn test_parse_blob_endpoint() {
        let endpoint =
            parse_v2_path("library/ubuntu/blobs/sha256:abcdef1234567890abcdef1234567890");
        assert_eq!(
            endpoint,
            V2Endpoint::Blob {
                name: "library/ubuntu".to_string(),
                digest: "sha256:abcdef1234567890abcdef1234567890".to_string()
            }
        );

        // Test with nested repository names
        let endpoint = parse_v2_path("ghcr.io/owner/repo/blobs/sha256:fedcba0987654321");
        assert_eq!(
            endpoint,
            V2Endpoint::Blob {
                name: "ghcr.io/owner/repo".to_string(),
                digest: "sha256:fedcba0987654321".to_string()
            }
        );
    }

    #[test]
    fn test_parse_blob_upload_init() {
        let endpoint = parse_v2_path("library/ubuntu/blobs/uploads");
        assert_eq!(
            endpoint,
            V2Endpoint::BlobUploadInit {
                name: "library/ubuntu".to_string()
            }
        );

        let endpoint = parse_v2_path("ghcr.io/owner/repo/blobs/uploads");
        assert_eq!(
            endpoint,
            V2Endpoint::BlobUploadInit {
                name: "ghcr.io/owner/repo".to_string()
            }
        );
    }

    #[test]
    fn test_parse_blob_upload_complete() {
        let endpoint =
            parse_v2_path("library/ubuntu/blobs/uploads/550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            endpoint,
            V2Endpoint::BlobUploadComplete {
                name: "library/ubuntu".to_string(),
                uuid: "550e8400-e29b-41d4-a716-446655440000".to_string()
            }
        );
    }

    #[test]
    fn test_parse_unknown_endpoint() {
        let endpoint = parse_v2_path("invalid/path");
        assert_eq!(endpoint, V2Endpoint::Unknown);

        let endpoint = parse_v2_path("");
        assert_eq!(endpoint, V2Endpoint::Unknown);

        let endpoint = parse_v2_path("library/ubuntu");
        assert_eq!(endpoint, V2Endpoint::Unknown);
    }

    #[test]
    fn test_parse_edge_cases() {
        // Manifest without reference
        let endpoint = parse_v2_path("library/ubuntu/manifests");
        assert_eq!(endpoint, V2Endpoint::Unknown);

        // Blob without digest
        let endpoint = parse_v2_path("library/ubuntu/blobs");
        assert_eq!(endpoint, V2Endpoint::Unknown);

        // Single part name
        let endpoint = parse_v2_path("ubuntu/manifests/latest");
        assert_eq!(
            endpoint,
            V2Endpoint::Manifest {
                name: "ubuntu".to_string(),
                reference: "latest".to_string()
            }
        );
    }
}
