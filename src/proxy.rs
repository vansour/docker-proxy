use crate::config::Config;
use crate::error::{ProxyError, ProxyResult};
use bytes::Bytes;
use reqwest::Method;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub struct DockerProxy {
    client: reqwest::Client,
    registry_url: String,
    ghcr_token: String,
}

impl DockerProxy {
    pub fn new(config: &Config) -> Self {
        let mut registry_url = config.default_registry().to_string();
        if !registry_url.starts_with("http") {
            registry_url = format!("https://{}", registry_url);
        }

        Self {
            client: reqwest::Client::new(),
            registry_url,
            ghcr_token: config.ghcr_token().to_string(),
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

    pub async fn get_blob(&self, name: &str, digest: &str) -> ProxyResult<Bytes> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);

        tracing::info!(
            registry = %registry_url,
            image = %image_name,
            digest = %digest,
            "Fetching blob"
        );

        let response = self.fetch_with_auth(Method::GET, &url, None).await?;

        if !response.status().is_success() {
            return Err(ProxyError::BlobNotFound {
                status: response.status(),
            });
        }

        let body = response
            .bytes()
            .await
            .map_err(|e| ProxyError::ResponseReadError(e.to_string()))?;

        Ok(body)
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

    // Helper: perform request and handle Docker Registry Bearer auth flow (WWW-Authenticate -> token)
    async fn fetch_with_auth(
        &self,
        method: Method,
        url: &str,
        extra_headers: Option<Vec<(&str, &str)>>,
    ) -> ProxyResult<reqwest::Response> {
        // Check if this is a GHCR request and we have a token
        let is_ghcr = self.is_ghcr_registry(url);
        let has_ghcr_token = is_ghcr && !self.ghcr_token.is_empty();

        // initial request
        let mut req = self.client.request(method.clone(), url);
        if let Some(hs) = &extra_headers {
            for (k, v) in hs.iter() {
                req = req.header(*k, *v);
            }
        }

        // Add GHCR token to initial request if available
        if has_ghcr_token {
            tracing::debug!("Using GHCR token for initial request");
            req = req.bearer_auth(&self.ghcr_token);
        }

        let resp = req.send().await?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }

        // parse WWW-Authenticate
        let www = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .ok_or(ProxyError::MissingAuthHeader)?;

        let params = Self::parse_www_authenticate(www);
        let realm = params.get("realm").ok_or(ProxyError::MissingAuthRealm)?;

        // build token request URL
        let mut token_url = realm.clone();
        if let Some(service) = params.get("service") {
            token_url.push_str(if token_url.contains('?') { "&" } else { "?" });
            token_url.push_str(&format!("service={}", service));
        }
        if let Some(scope) = params.get("scope") {
            token_url.push_str(if token_url.contains('?') { "&" } else { "?" });
            token_url.push_str(&format!("scope={}", scope));
        }

        tracing::info!(
            token_url = %token_url,
            has_auth = has_ghcr_token,
            "Requesting authentication token"
        );

        // Build token request with GHCR authentication if available
        let mut token_req = self.client.get(&token_url);
        if has_ghcr_token {
            tracing::debug!("Using GHCR token for authentication");
            token_req = token_req.bearer_auth(&self.ghcr_token);
        }

        let token_resp = token_req.send().await?;

        if !token_resp.status().is_success() {
            return Err(ProxyError::TokenRequestFailed {
                status: token_resp.status(),
            });
        }

        let j: JsonValue = token_resp
            .json()
            .await
            .map_err(|e| ProxyError::TokenParseFailed(e.to_string()))?;

        let token = j
            .get("token")
            .and_then(|v| v.as_str())
            .or_else(|| j.get("access_token").and_then(|v| v.as_str()))
            .ok_or(ProxyError::TokenNotFound)?;

        // retry original request with Authorization
        let mut req2 = self.client.request(method, url).bearer_auth(token);
        if let Some(hs) = &extra_headers {
            for (k, v) in hs.iter() {
                req2 = req2.header(*k, *v);
            }
        }

        let resp2 = req2.send().await?;

        Ok(resp2)
    }

    // Check if a URL belongs to GitHub Container Registry
    fn is_ghcr_registry(&self, url: &str) -> bool {
        url.contains("ghcr.io")
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

    // parse header like: Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull"
    fn parse_www_authenticate(s: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        // find the part after the auth scheme (e.g., "Bearer ")
        let parts: Vec<&str> = s.splitn(2, ' ').collect();
        if parts.len() < 2 {
            return out;
        }
        let params_part = parts[1];
        for pair in params_part.split(',') {
            let kv: Vec<&str> = pair.splitn(2, '=').collect();
            if kv.len() != 2 {
                continue;
            }
            let key = kv[0].trim().trim_matches(',').trim().to_string();
            let mut val = kv[1].trim().to_string();
            if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                val = val[1..val.len() - 1].to_string();
            }
            out.insert(key, val);
        }
        out
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
    fn test_is_ghcr_registry() {
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
ghcr-token = "test_token"
"#,
        )
        .expect("Failed to parse test config");

        let proxy = DockerProxy::new(&config);

        assert!(proxy.is_ghcr_registry("https://ghcr.io/v2/test"));
        assert!(proxy.is_ghcr_registry("https://ghcr.io/owner/repo"));
        assert!(!proxy.is_ghcr_registry("https://docker.io/v2/test"));
        assert!(!proxy.is_ghcr_registry("https://registry-1.docker.io/v2/test"));
    }

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

    #[test]
    fn test_parse_www_authenticate() {
        // Test standard Docker Hub auth header
        let header = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull""#;
        let params = DockerProxy::parse_www_authenticate(header);

        assert_eq!(
            params.get("realm"),
            Some(&"https://auth.docker.io/token".to_string())
        );
        assert_eq!(
            params.get("service"),
            Some(&"registry.docker.io".to_string())
        );
        assert_eq!(
            params.get("scope"),
            Some(&"repository:library/ubuntu:pull".to_string())
        );

        // Test GHCR auth header
        let ghcr_header = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:vansour/docker-proxy:pull""#;
        let ghcr_params = DockerProxy::parse_www_authenticate(ghcr_header);

        assert_eq!(
            ghcr_params.get("realm"),
            Some(&"https://ghcr.io/token".to_string())
        );
        assert_eq!(ghcr_params.get("service"), Some(&"ghcr.io".to_string()));
    }

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
