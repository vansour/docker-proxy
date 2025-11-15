use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl ServerConfig {
    /// Validate server configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty() {
            return Err("Server host cannot be empty".to_string());
        }
        if self.port == 0 {
            return Err("Server port must be greater than 0".to_string());
        }
        Ok(())
    }

    /// Get socket address
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(rename = "logFilePath")]
    pub log_file_path: String,
    pub level: String,
}

impl LogConfig {
    /// Validate log configuration
    pub fn validate(&self) -> Result<(), String> {
        let valid_levels = vec!["debug", "info", "warn", "error", "trace"];
        if !valid_levels.contains(&self.level.to_lowercase().as_str()) {
            return Err(format!(
                "Invalid log level '{}'. Must be one of: {:?}",
                self.level, valid_levels
            ));
        }
        if self.log_file_path.is_empty() {
            return Err("Log file path cannot be empty".to_string());
        }
        Ok(())
    }

    /// Get normalized log level
    pub fn normalized_level(&self) -> String {
        self.level.to_lowercase()
    }
}

/// Proxy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub default: String,
}

impl ProxyConfig {
    /// Validate proxy configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.default.is_empty() {
            return Err("Default proxy registry cannot be empty".to_string());
        }
        Ok(())
    }
}

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(rename = "ghcr-token")]
    pub ghcr_token: String,
}

impl AuthConfig {
    /// Check if GHCR token is configured
    #[allow(dead_code)]
    pub fn has_ghcr_token(&self) -> bool {
        !self.ghcr_token.is_empty()
    }
}

/// Root configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub log: LogConfig,
    pub proxy: ProxyConfig,
    pub auth: AuthConfig,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(format!("Configuration file not found: {:?}", path).into());
        }
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Load configuration from a string
    #[allow(dead_code)]
    pub fn from_str(content: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config: Config = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the entire configuration
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.server.validate()?;
        self.log.validate()?;
        self.proxy.validate()?;
        Ok(())
    }

    /// Get the server address as a string
    pub fn server_addr(&self) -> String {
        self.server.socket_addr()
    }

    /// Get the default registry proxy
    pub fn default_registry(&self) -> &str {
        &self.proxy.default
    }

    /// Get the logging level
    pub fn log_level(&self) -> &str {
        &self.log.level
    }

    /// Get the normalized logging level (lowercase)
    pub fn log_level_normalized(&self) -> String {
        self.log.normalized_level()
    }

    /// Get the log file path
    pub fn log_file_path(&self) -> &str {
        &self.log.log_file_path
    }

    /// Get the GHCR authentication token
    #[allow(dead_code)]
    pub fn ghcr_token(&self) -> &str {
        &self.auth.ghcr_token
    }

    /// Check if GHCR token is configured
    #[allow(dead_code)]
    pub fn has_ghcr_token(&self) -> bool {
        self.auth.has_ghcr_token()
    }

    /// Convert to a display string with masked sensitive data
    pub fn to_display_string(&self) -> String {
        format!(
            "Server: {} | Log Level: {} | Log Path: {} | Default Registry: {}",
            self.server_addr(),
            self.log_level(),
            self.log_file_path(),
            self.default_registry()
        )
    }
}
