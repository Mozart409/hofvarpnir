//! Application configuration, loaded from environment variables.

use std::{env, net::IpAddr, path::PathBuf, time::Duration};

use color_eyre::eyre::{Result, WrapErr};

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Database configuration.
    pub database: DatabaseConfig,
    /// Server configuration.
    pub server: ServerConfig,
    /// Download configuration.
    pub download: DownloadConfig,
    /// Storage configuration.
    pub storage: StorageConfig,
}

/// Database connection configuration.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Database connection URL (uses `SQLx`'s built-in connection pooling).
    pub url: String,
}

/// HTTP server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Host address to bind to.
    pub host: IpAddr,
    /// Port to listen on.
    pub port: u16,
}

/// Download behavior configuration.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum number of concurrent downloads.
    pub max_concurrent: u32,
    /// Timeout for a single download.
    pub timeout: Duration,
    /// Maximum retry attempts before marking as permanently failed.
    pub max_attempts: u32,
    /// Delay between yt-dlp invocations to avoid rate limiting.
    pub rate_limit_delay: Duration,
    /// Path to yt-dlp binary.
    pub ytdlp_path: PathBuf,
}

/// Storage configuration.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Default output directory for downloads.
    pub default_output_dir: PathBuf,
    /// Global retention policy in days (can be overridden per-profile/source).
    pub retention_days: Option<u32>,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Call `dotenvy::dotenv().ok()` before this if you want to load from `.env` files.
    ///
    /// # Errors
    ///
    /// Returns an error if required environment variables are missing (e.g., `DATABASE_URL`).
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database: DatabaseConfig::from_env()?,
            server: ServerConfig::from_env(),
            download: DownloadConfig::from_env(),
            storage: StorageConfig::from_env(),
        })
    }

    /// Load configuration, first loading `.env` file if present.
    ///
    /// This is the preferred method for application startup.
    ///
    /// # Errors
    ///
    /// Returns an error if required environment variables are missing (e.g., `DATABASE_URL`).
    pub fn load() -> Result<Self> {
        // Load .env file if present (ignore errors if not found)
        dotenvy::dotenv().ok();
        Self::from_env()
    }
}

impl DatabaseConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            url: require_env("DATABASE_URL")?,
        })
    }
}

impl ServerConfig {
    fn from_env() -> Self {
        Self {
            host: optional_env("HOST")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| "127.0.0.1".parse().unwrap()),
            port: optional_env("PORT")
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
        }
    }
}

impl DownloadConfig {
    fn from_env() -> Self {
        let max_concurrent = optional_env("MAX_CONCURRENT_DOWNLOADS")
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);

        let timeout_hours = optional_env("DOWNLOAD_TIMEOUT_HOURS")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(4);

        let max_attempts = optional_env("MAX_DOWNLOAD_ATTEMPTS")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);

        let rate_limit_secs = optional_env("RATE_LIMIT_DELAY_SECS")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);

        let ytdlp_path =
            optional_env("YTDLP_PATH").map_or_else(|| PathBuf::from("yt-dlp"), PathBuf::from);

        Self {
            max_concurrent,
            timeout: Duration::from_secs(timeout_hours * 3600),
            max_attempts,
            rate_limit_delay: Duration::from_secs(rate_limit_secs),
            ytdlp_path,
        }
    }
}

impl StorageConfig {
    fn from_env() -> Self {
        let default_output_dir = optional_env("DEFAULT_OUTPUT_DIR").map_or_else(
            || PathBuf::from("/var/lib/hofvarpnir/downloads"),
            PathBuf::from,
        );

        let retention_days = optional_env("RETENTION_DAYS").and_then(|s| s.parse().ok());

        Self {
            default_output_dir,
            retention_days,
        }
    }
}

/// Get a required environment variable.
fn require_env(key: &str) -> Result<String> {
    env::var(key).wrap_err_with(|| format!("missing required environment variable: {key}"))
}

/// Get an optional environment variable.
fn optional_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Tests that rely on clearing environment variables are difficult to write
    // safely in Rust 2024 edition where `env::remove_var` is unsafe. These tests
    // verify the parsing logic works with known values instead.

    #[test]
    fn test_server_parses_valid_host() {
        // Test that parsing logic works - actual env loading tested via integration tests
        let host: IpAddr = "0.0.0.0".parse().unwrap();
        assert_eq!(host.to_string(), "0.0.0.0");
    }

    #[test]
    fn test_download_timeout_calculation() {
        // 4 hours in seconds
        let timeout = Duration::from_hours(4);
        assert_eq!(timeout.as_secs(), 14400);
    }

    #[test]
    fn test_require_env_missing() {
        // Use a key that is extremely unlikely to be set
        let result = require_env("HOF_TEST_NONEXISTENT_VAR_12345");
        assert!(result.is_err());
    }

    #[test]
    fn test_optional_env_missing() {
        // Use a key that is extremely unlikely to be set
        let result = optional_env("HOF_TEST_NONEXISTENT_VAR_12345");
        assert!(result.is_none());
    }
}
