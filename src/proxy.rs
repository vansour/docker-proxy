use crate::config::Config;
use crate::error::{ProxyError, ProxyResult};
use reqwest::Method;
use serde_json::Value as JsonValue;

pub struct DockerProxy {
    client: reqwest::Client,
    registry_url: String,
}

impl DockerProxy {
    pub fn new(config: &Config) -> Self {
        // Normalize default registry URL from config
        let mut registry_url = config.default_registry().to_string();
        if !registry_url.starts_with("http://") && !registry_url.starts_with("https://") {
            registry_url = format!("https://{}", registry_url);
        }

        // Build client without automatic content decoding to preserve blob sizes
        let client = reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build custom client, using default: {}", e);
                reqwest::Client::new()
            });

        Self {
            client,
            registry_url,
        }
    }

    pub async fn get_manifest(&self, name: &str, reference: &str) -> ProxyResult<(String, String)> {
        // allow name to include a registry prefix (e.g. "ghcr.io/vansour/gh-proxy")
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/manifests/{}", registry_url, image_name, reference);

        tracing::info!(
            registry = %registry_url,
            image = %image_name,
            reference = %reference,
            "Fetching manifest"
        );

        let response = self
            .fetch_with_auth(
                Method::GET,
                &url,
                Some(vec![
                    (
                        "Accept",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    ),
                    (
                        "Accept",
                        "application/vnd.docker.distribution.manifest.list.v2+json",
                    ),
                ]),
            )
            .await?;

        if !response.status().is_success() {
            return Err(ProxyError::ManifestNotFound {
                status: response.status(),
            });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        let body = response
            .text()
            .await
            .map_err(|e| ProxyError::ResponseReadError(e.to_string()))?;

        Ok((content_type, body))
    }

    pub async fn head_manifest(&self, name: &str, reference: &str) -> ProxyResult<(String, u64)> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/manifests/{}", registry_url, image_name, reference);

        tracing::info!(
            registry = %registry_url,
            image = %image_name,
            reference = %reference,
            "HEAD request for manifest"
        );

        let response = self
            .fetch_with_auth(
                Method::HEAD,
                &url,
                Some(vec![(
                    "Accept",
                    "application/vnd.docker.distribution.manifest.v2+json",
                )]),
            )
            .await?;

        if !response.status().is_success() {
            return Err(ProxyError::ManifestNotFound {
                status: response.status(),
            });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok((content_type, content_length))
    }

    pub async fn get_blob(&self, name: &str, digest: &str) -> ProxyResult<reqwest::Response> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);

        tracing::info!(
            registry = %registry_url,
            image = %image_name,
            digest = %digest,
            "Fetching blob"
        );

        let response = self.fetch_with_auth(Method::GET, &url, None).await?;

        // 始终返回上游响应，由上层根据状态码决定如何处理
        Ok(response)
    }

    pub async fn head_blob(&self, name: &str, digest: &str) -> ProxyResult<u64> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);

        tracing::info!(
            registry = %registry_url,
            image = %image_name,
            digest = %digest,
            "HEAD request for blob"
        );

        let response = self.fetch_with_auth(Method::HEAD, &url, None).await?;

        if !response.status().is_success() {
            return Err(ProxyError::BlobNotFound {
                status: response.status(),
            });
        }

        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(content_length)
    }

    /// 调试用：获取指定镜像+digest 的 manifest size 和实际 blob 大小
    pub async fn debug_blob_info(
        &self,
        name: &str,
        digest: &str,
        reference: &str,
    ) -> ProxyResult<(u64, u64)> {
        // 1. 获取 manifest（v2 schema）并解析 size
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let manifest_url = format!("{}/v2/{}/manifests/{}", registry_url, image_name, reference);

        let manifest_resp = self
            .fetch_with_auth(
                Method::GET,
                &manifest_url,
                Some(vec![
                    (
                        "Accept",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    ),
                    (
                        "Accept",
                        "application/vnd.docker.distribution.manifest.list.v2+json",
                    ),
                ]),
            )
            .await?;

        if !manifest_resp.status().is_success() {
            return Err(ProxyError::ManifestNotFound {
                status: manifest_resp.status(),
            });
        }

        let manifest_json: JsonValue = manifest_resp
            .json()
            .await
            .map_err(|e| ProxyError::ResponseReadError(e.to_string()))?;

        // manifest 可能是 manifest list，需要选中对应平台；简单起见先按普通 manifest 处理
        let mut manifest_size: u64 = 0;
        if let Some(layers) = manifest_json.get("layers").and_then(|v| v.as_array()) {
            for layer in layers {
                if let Some(d) = layer.get("digest").and_then(|v| v.as_str())
                    && d == digest
                {
                    if let Some(s) = layer.get("size").and_then(|v| v.as_u64()) {
                        manifest_size = s;
                    }
                    break;
                }
            }
        }

        // 2. 获取 blob，统计实际字节数
        let blob_url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);
        let blob_resp = self.fetch_with_auth(Method::GET, &blob_url, None).await?;

        if !blob_resp.status().is_success() {
            return Err(ProxyError::BlobNotFound {
                status: blob_resp.status(),
            });
        }

        let mut stream = blob_resp.bytes_stream();
        let mut actual_size: u64 = 0;

        use futures_util::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(ProxyError::Network)?;
            actual_size += bytes.len() as u64;
        }

        Ok((manifest_size, actual_size))
    }

    pub async fn initiate_blob_upload(&self, _name: &str) -> ProxyResult<String> {
        Err(ProxyError::BlobUploadNotSupported)
    }

    /// Check health of the default registry
    /// Returns true if the registry is reachable and responding
    pub async fn check_registry_health(&self) -> bool {
        let url = format!("{}/v2/", self.registry_url);

        match self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => {
                // Registry should return 200 or 401 (401 means it's working but needs auth)
                let status = resp.status();
                status.is_success() || status == reqwest::StatusCode::UNAUTHORIZED
            }
            Err(e) => {
                tracing::warn!("Registry health check failed: {}", e);
                false
            }
        }
    }

    /// Get the default registry URL
    pub fn get_registry_url(&self) -> &str {
        &self.registry_url
    }

    // Helper: perform a simple HTTP request with optional extra headers (no auth handling)
    async fn fetch_with_auth(
        &self,
        method: Method,
        url: &str,
        extra_headers: Option<Vec<(&str, &str)>>,
    ) -> ProxyResult<reqwest::Response> {
        let mut req = self.client.request(method, url);
        if let Some(hs) = &extra_headers {
            for (k, v) in hs.iter() {
                req = req.header(*k, *v);
            }
        }

        let resp = req.send().await?;
        Ok(resp)
    }

    // If `name` is like "ghcr.io/owner/repo" return ("https://ghcr.io", "owner/repo")
    // Otherwise return (self.registry_url.clone(), normalized_name)
    fn split_registry_and_name(&self, name: &str) -> (String, String) {
        if let Some(pos) = name.find('/') {
            let first = &name[..pos];
            // treat as registry when first segment looks like a host (contains dot or colon)
            if first.contains('.') || first.contains(':') {
                let registry_url = format!("https://{}", first);
                let rest = &name[pos + 1..];
                return (registry_url, rest.to_string());
            }
        }
        (self.registry_url.clone(), self.normalize_image_name(name))
    }

    // 规范化镜像名称：如果没有指定registry，默认使用library
    fn normalize_image_name(&self, name: &str) -> String {
        if name.contains('/') {
            name.to_string()
        } else {
            format!("library/{}", name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_registry_and_name() {
        let config = Config::from_str(
            r#"
[server]
host = "0.0.0.0"
port = 8080

[log]
logFilePath = "/tmp/test.log"
level = "info"

[proxy]
default = "docker.io"

[auth]
ghcr-token = ""
"#,
        )
        .expect("Failed to parse test config");

        let proxy = DockerProxy::new(&config);

        // Test with explicit registry
        let (registry, name) = proxy.split_registry_and_name("ghcr.io/vansour/docker-proxy");
        assert_eq!(registry, "https://ghcr.io");
        assert_eq!(name, "vansour/docker-proxy");

        // Test with docker.io registry
        let (registry, name) = proxy.split_registry_and_name("docker.io/library/ubuntu");
        assert_eq!(registry, "https://docker.io");
        assert_eq!(name, "library/ubuntu");

        // Test without registry (should use default and add library prefix)
        let (registry, name) = proxy.split_registry_and_name("ubuntu");
        assert_eq!(registry, "https://docker.io");
        assert_eq!(name, "library/ubuntu");

        // Test with owner/repo format
        let (registry, name) = proxy.split_registry_and_name("vansour/myimage");
        assert_eq!(registry, "https://docker.io");
        assert_eq!(name, "vansour/myimage");
    }

    #[test]
    fn test_normalize_image_name() {
        let config = Config::from_str(
            r#"
[server]
host = "0.0.0.0"
port = 8080

[log]
logFilePath = "/tmp/test.log"
level = "info"

[proxy]
default = "docker.io"

[auth]
ghcr-token = ""
"#,
        )
        .expect("Failed to parse test config");

        let proxy = DockerProxy::new(&config);

        // Single name should get library prefix
        assert_eq!(proxy.normalize_image_name("ubuntu"), "library/ubuntu");
        assert_eq!(proxy.normalize_image_name("nginx"), "library/nginx");

        // Name with slash should remain unchanged
        assert_eq!(
            proxy.normalize_image_name("vansour/docker-proxy"),
            "vansour/docker-proxy"
        );
        assert_eq!(
            proxy.normalize_image_name("library/ubuntu"),
            "library/ubuntu"
        );
    }

    // auth-related parsing tests removed because proxy no longer handles auth

    #[test]
    fn test_get_registry_url() {
        let config = Config::from_str(
            r#"
[server]
host = "0.0.0.0"
port = 8080

[log]
logFilePath = "/tmp/test.log"
level = "info"

[proxy]
default = "docker.io"

[auth]
ghcr-token = ""
"#,
        )
        .expect("Failed to parse test config");

        let proxy = DockerProxy::new(&config);
        assert_eq!(proxy.get_registry_url(), "https://docker.io");
    }

    #[test]
    fn test_registry_url_normalization() {
        // Test with protocol
        let config1 = Config::from_str(
            r#"
[server]
host = "0.0.0.0"
port = 8080

[log]
logFilePath = "/tmp/test.log"
level = "info"

[proxy]
default = "https://ghcr.io"

[auth]
ghcr-token = ""
"#,
        )
        .expect("Failed to parse test config with protocol");

        let proxy1 = DockerProxy::new(&config1);
        assert_eq!(proxy1.get_registry_url(), "https://ghcr.io");

        // Test without protocol
        let config2 = Config::from_str(
            r#"
[server]
host = "0.0.0.0"
port = 8080

[log]
logFilePath = "/tmp/test.log"
level = "info"

[proxy]
default = "quay.io"

[auth]
ghcr-token = ""
"#,
        )
        .expect("Failed to parse test config without protocol");

        let proxy2 = DockerProxy::new(&config2);
        assert_eq!(proxy2.get_registry_url(), "https://quay.io");
    }
}
