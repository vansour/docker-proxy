use bytes::Bytes;
use reqwest::Method;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

pub struct DockerProxy {
    client: reqwest::Client,
    registry_url: String,
}

impl DockerProxy {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            registry_url: "https://registry-1.docker.io".to_string(),
        }
    }

    pub async fn get_manifest(
        &self,
        name: &str,
        reference: &str,
    ) -> Result<(String, String), String> {
        // allow name to include a registry prefix (e.g. "ghcr.io/vansour/gh-proxy")
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/manifests/{}", registry_url, image_name, reference);

        info!("Fetching manifest from: {}", url);

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
            .await
            .map_err(|e| format!("Failed to fetch manifest: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Manifest not found: {}", response.status()));
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
            .map_err(|e| format!("Failed to read response: {}", e))?;

        Ok((content_type, body))
    }

    pub async fn head_manifest(
        &self,
        name: &str,
        reference: &str,
    ) -> Result<(String, u64), String> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/manifests/{}", registry_url, image_name, reference);

        info!("HEAD request for manifest: {}", url);

        let response = self
            .fetch_with_auth(
                Method::HEAD,
                &url,
                Some(vec![(
                    "Accept",
                    "application/vnd.docker.distribution.manifest.v2+json",
                )]),
            )
            .await
            .map_err(|e| format!("Failed to HEAD manifest: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Manifest not found: {}", response.status()));
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

    pub async fn get_blob(&self, name: &str, digest: &str) -> Result<Bytes, String> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);

        info!("Fetching blob from: {}", url);

        let response = self
            .fetch_with_auth(Method::GET, &url, None)
            .await
            .map_err(|e| format!("Failed to fetch blob: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Blob not found: {}", response.status()));
        }

        let body = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read blob bytes: {}", e))?;

        Ok(body)
    }

    pub async fn head_blob(&self, name: &str, digest: &str) -> Result<u64, String> {
        let (registry_url, image_name) = self.split_registry_and_name(name);
        let url = format!("{}/v2/{}/blobs/{}", registry_url, image_name, digest);

        info!("HEAD request for blob: {}", url);

        let response = self
            .fetch_with_auth(Method::HEAD, &url, None)
            .await
            .map_err(|e| format!("Failed to HEAD blob: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Blob not found: {}", response.status()));
        }

        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(content_length)
    }

    pub async fn initiate_blob_upload(&self, _name: &str) -> Result<String, String> {
        Ok(Uuid::new_v4().to_string())
    }

    // Helper: perform request and handle Docker Registry Bearer auth flow (WWW-Authenticate -> token)
    async fn fetch_with_auth(
        &self,
        method: Method,
        url: &str,
        extra_headers: Option<Vec<(&str, &str)>>,
    ) -> Result<reqwest::Response, String> {
        // initial request
        let mut req = self.client.request(method.clone(), url);
        if let Some(hs) = &extra_headers {
            for (k, v) in hs.iter() {
                req = req.header(*k, *v);
            }
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("request error: {}", e))?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }

        // parse WWW-Authenticate
        let www = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| "Unauthorized and missing WWW-Authenticate header".to_string())?;

        let params = Self::parse_www_authenticate(www);
        let realm = params
            .get("realm")
            .ok_or_else(|| "WWW-Authenticate missing realm".to_string())?;

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

        info!("Requesting token from: {}", token_url);

        let token_resp = self
            .client
            .get(&token_url)
            .send()
            .await
            .map_err(|e| format!("Failed to request token: {}", e))?;

        if !token_resp.status().is_success() {
            return Err(format!("Token request failed: {}", token_resp.status()));
        }

        let j: JsonValue = token_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse token response: {}", e))?;

        let token = j
            .get("token")
            .and_then(|v| v.as_str())
            .or_else(|| j.get("access_token").and_then(|v| v.as_str()))
            .ok_or_else(|| "token not found in token response".to_string())?;

        // retry original request with Authorization
        let mut req2 = self.client.request(method, url).bearer_auth(token);
        if let Some(hs) = &extra_headers {
            for (k, v) in hs.iter() {
                req2 = req2.header(*k, *v);
            }
        }

        let resp2 = req2
            .send()
            .await
            .map_err(|e| format!("retry request error: {}", e))?;

        Ok(resp2)
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
