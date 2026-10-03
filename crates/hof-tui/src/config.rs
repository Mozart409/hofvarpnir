//! Runtime configuration for the TUI: API base URL and API token.
//!
//! Resolution order (CLI wins over env):
//! - `--api-url <URL>` / `HOF_API_URL` / default `http://localhost:3000`
//! - `--token <TOKEN>` / `HOF_API_TOKEN` (must be a `hof_sk_...` key; when
//!   absent, the TUI asks for URL and token on a setup screen)

/// Default server URL when neither `--api-url` nor `HOF_API_URL` is set.
/// Matches the server's default `PORT` (3000).
const DEFAULT_API_URL: &str = "http://localhost:3000";

/// Every API key minted by Hofvarpnir carries this prefix; the server rejects
/// anything else, so fail fast locally instead of round-tripping a 401.
const TOKEN_PREFIX: &str = "hof_sk_";

/// Resolved TUI configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Server base URL, no trailing slash (e.g. `http://localhost:3000`).
    pub api_url: String,
    /// API key sent as `Authorization: Bearer <token>`.
    pub token: String,
}

/// Configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `--help` was passed; not an error, carries the usage text.
    #[error("{0}")]
    Help(String),
    #[error("unknown argument: {0}")]
    UnknownArg(String),
    #[error("missing value for {0}")]
    MissingValue(&'static str),
    #[error(
        "no API token: pass --token or set HOF_API_TOKEN (create one in the web UI under Settings)"
    )]
    MissingToken,
    #[error("invalid API token: expected it to start with `{TOKEN_PREFIX}`")]
    InvalidToken,
}

const USAGE: &str = "\
hofvarpnir-tui — terminal client for the Hofvarpnir API

USAGE:
    hofvarpnir-tui [OPTIONS]

OPTIONS:
    --api-url <URL>   Base URL of the Hofvarpnir server
                      [env: HOF_API_URL] [default: http://localhost:3000]
    --token <TOKEN>   API key (hof_sk_...) [env: HOF_API_TOKEN]
                      When absent, a setup screen asks for URL and token.
    -h, --help        Print this help
";

/// Arguments and env resolved, but not yet validated as a complete
/// [`Config`]: the token may be absent, in which case the TUI opens its
/// setup screen instead of exiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialConfig {
    /// Normalized server URL (default applied, trailing slash stripped).
    pub api_url: String,
    /// Token from `--token` / `HOF_API_TOKEN`, if any and non-empty.
    pub token: Option<String>,
}

impl PartialConfig {
    /// Resolve process args (excluding argv[0]) and env without requiring a
    /// token.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` on unknown args, missing values, or `--help`.
    pub fn from_args_and_env<I, S>(args: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut api_url = std::env::var("HOF_API_URL").ok();
        let mut token = std::env::var("HOF_API_TOKEN").ok();

        let mut args = args.into_iter().map(Into::into);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => return Err(ConfigError::Help(USAGE.to_string())),
                "--api-url" => {
                    api_url = Some(args.next().ok_or(ConfigError::MissingValue("--api-url"))?);
                }
                "--token" => {
                    token = Some(args.next().ok_or(ConfigError::MissingValue("--token"))?);
                }
                other => return Err(ConfigError::UnknownArg(other.to_string())),
            }
        }

        Ok(Self {
            api_url: normalize_url(api_url.as_deref().unwrap_or_default()),
            token: token.filter(|t| !t.is_empty()),
        })
    }

    /// Complete into a [`Config`].
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` for a missing or malformed token.
    pub fn into_config(self) -> Result<Config, ConfigError> {
        let token = self.token.ok_or(ConfigError::MissingToken)?;
        Config::new(&self.api_url, &token)
    }
}

/// Trim whitespace and trailing slashes; empty means [`DEFAULT_API_URL`].
/// A bare `host:port` gets `http://` prepended, since reqwest rejects a URL
/// without a scheme with an opaque "builder error".
#[must_use]
pub fn normalize_url(raw: &str) -> String {
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() {
        DEFAULT_API_URL.to_string()
    } else if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

impl Config {
    /// Build from a URL and token as typed by the user (CLI, env, or the
    /// setup screen). Whitespace around either is ignored.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` for an empty or non-`hof_sk_` token.
    pub fn new(api_url: &str, token: &str) -> Result<Self, ConfigError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(ConfigError::MissingToken);
        }
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(ConfigError::InvalidToken);
        }
        Ok(Self {
            api_url: normalize_url(api_url),
            token: token.to_string(),
        })
    }

    /// Resolve configuration from process args (excluding argv[0]) and env.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` on unknown args, missing values, or a
    /// missing/malformed token.
    pub fn from_args_and_env<I, S>(args: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        PartialConfig::from_args_and_env(args)?.into_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_token() {
        // Env may legitimately carry HOF_API_TOKEN on a dev machine, so only
        // assert the error kind when it is absent.
        if std::env::var("HOF_API_TOKEN").is_err() {
            let err = Config::from_args_and_env(Vec::<String>::new()).unwrap_err();
            assert!(matches!(err, ConfigError::MissingToken));
        }
    }

    #[test]
    fn rejects_non_prefixed_token() {
        let err =
            Config::from_args_and_env(["--token".to_string(), "nope".to_string()]).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken));
    }

    #[test]
    fn strips_trailing_slash_from_url() {
        let cfg = Config::from_args_and_env([
            "--api-url".to_string(),
            "http://example:3000/".to_string(),
            "--token".to_string(),
            "hof_sk_abc".to_string(),
        ])
        .unwrap();
        assert_eq!(cfg.api_url, "http://example:3000");
        assert_eq!(cfg.token, "hof_sk_abc");
    }

    #[test]
    fn help_is_not_an_error_with_side_effects() {
        let err = Config::from_args_and_env(["--help".to_string()]).unwrap_err();
        assert!(matches!(err, ConfigError::Help(_)));
    }
}
