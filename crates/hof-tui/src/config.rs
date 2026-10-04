//! Runtime configuration for the TUI: API base URL and API token.
//!
//! Resolution order (CLI wins over env, env wins over the config file):
//! - `--api-url <URL>` / `HOF_API_URL` / `api_url` / default `http://localhost:8080`
//! - `--token <TOKEN>` / `HOF_API_TOKEN` / one of `token`, `token_file`,
//!   `token_command` (must yield a `hof_sk_...` key; when absent, the TUI
//!   asks for URL and token on a setup screen)
//!
//! The config file is `--config <PATH>` / `HOF_TUI_CONFIG` /
//! `$XDG_CONFIG_HOME/hofvarpnir/tui.toml` / `~/.config/hofvarpnir/tui.toml`.
//! `token_file` and `token_command` keep the secret out of the file, so it
//! can live in a read-only, world-readable place such as a home-manager
//! generated `~/.config` on `NixOS`, with the key supplied by agenix or sops.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use serde::{Deserialize, Serialize};

/// Default server URL when neither `--api-url`, `HOF_API_URL`, nor the
/// config file sets one. The port the development server is run on locally;
/// the server's own compiled-in `PORT` default (3000) is not what dev setups
/// use.
const DEFAULT_API_URL: &str = "http://localhost:8080";

/// Every API key minted by Hofvarpnir carries this prefix; the server rejects
/// anything else, so fail fast locally instead of round-tripping a 401.
const TOKEN_PREFIX: &str = "hof_sk_";

/// First line of a config file written by the setup screen.
const SAVED_HEADER: &str = "# Written by hofvarpnir-tui. Holds an API key: keep it mode 600.\n";

/// Resolved TUI configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Server base URL, no trailing slash (e.g. `http://localhost:8080`).
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
        "no API token: pass --token, set HOF_API_TOKEN, or add a token source to the config file \
         (see --help; create a key in the web UI under Settings)"
    )]
    MissingToken,
    #[error("invalid API token: expected it to start with `{TOKEN_PREFIX}`")]
    InvalidToken,
    #[error("config file {} not found", .0.display())]
    FileNotFound(PathBuf),
    #[error("cannot read config file {}: {source}", path.display())]
    ReadFile { path: PathBuf, source: io::Error },
    #[error("invalid config file {}: {message}", path.display())]
    ParseFile { path: PathBuf, message: String },
    #[error(
        "config file {} sets more than one of `token`, `token_file`, `token_command`; keep one",
        .0.display()
    )]
    ConflictingTokenSources(PathBuf),
    #[error(
        "config file {} holds a `token` but is readable by other users (mode {mode:o}); \
         run `chmod 600` on it, or use `token_file` / `token_command` instead",
        path.display()
    )]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("cannot read token_file {}: {source}", path.display())]
    TokenFile { path: PathBuf, source: io::Error },
    #[error("cannot start token_command `{command}`: {source}")]
    TokenCommandSpawn { command: String, source: io::Error },
    #[error("token_command `{command}` failed ({status})")]
    TokenCommandFailed { command: String, status: ExitStatus },
    #[error("token_command `{0}` printed non-UTF-8 output")]
    TokenCommandNotUtf8(String),
    /// The payload names where the token came from.
    #[error("{0} is empty")]
    EmptyToken(String),
}

const USAGE: &str = "\
hofvarpnir-tui — terminal client for the Hofvarpnir API

USAGE:
    hofvarpnir-tui [OPTIONS]

OPTIONS:
    --api-url <URL>   Base URL of the Hofvarpnir server
                      [env: HOF_API_URL] [default: http://localhost:8080]
    --token <TOKEN>   API key (hof_sk_...) [env: HOF_API_TOKEN]
    --config <PATH>   Config file [env: HOF_TUI_CONFIG]
                      [default: $XDG_CONFIG_HOME/hofvarpnir/tui.toml]
    -h, --help        Print this help

Options and env win over the config file. When no token comes from any of
them, a setup screen asks for URL and token and can save them.

CONFIG FILE (TOML, all keys optional, at most one token source):
    api_url = \"https://hof.example.com\"
    token = \"hof_sk_...\"                       # file must be mode 600
    token_file = \"/run/agenix/hof-api-token\"   # e.g. agenix or sops-nix
    token_command = \"pass show hofvarpnir\"     # run via sh -c; stdout is the token
";

/// Arguments, env, and config file resolved, but not yet validated as a
/// complete [`Config`]: the token may be absent, in which case the TUI opens
/// its setup screen instead of exiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialConfig {
    /// Normalized server URL (default applied, trailing slash stripped).
    pub api_url: String,
    /// Token from `--token` / `HOF_API_TOKEN` / the config file, if any.
    pub token: Option<String>,
    /// Where the setup screen may save what the user enters: the config path
    /// when no file exists there yet. `None` when a file exists (it is never
    /// overwritten) or no path could be determined.
    pub save_path: Option<PathBuf>,
}

/// On-disk config file layout.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    api_url: Option<String>,
    token: Option<String>,
    token_file: Option<String>,
    token_command: Option<String>,
}

/// Where the config file says the token comes from.
#[derive(Debug)]
enum TokenSource {
    Inline(String),
    File(String),
    Command(String),
}

/// A parsed and validated config file.
#[derive(Debug)]
struct LoadedFile {
    path: PathBuf,
    api_url: Option<String>,
    token: Option<TokenSource>,
}

impl PartialConfig {
    /// Resolve process args (excluding argv[0]), the process environment, and
    /// the config file without requiring a token.
    ///
    /// # Errors
    ///
    /// See [`Self::resolve`].
    pub fn from_args_and_env<I, S>(args: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::resolve(args, &|key| std::env::var(key).ok())
    }

    /// Resolve args against an environment given as a lookup function, so
    /// callers (and tests) control which variables are visible.
    ///
    /// A token from `--token` / `HOF_API_TOKEN` skips the config file's token
    /// source entirely: a `token_command` only runs when its output is needed.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` on unknown args, missing values, or `--help`; for
    /// a config file that is missing (only when named explicitly), unreadable,
    /// malformed, or holds a `token` while readable by others; and when a
    /// `token_file` / `token_command` fails or yields nothing.
    pub fn resolve<I, S>(args: I, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut cli_url = None;
        let mut cli_token = None;
        let mut cli_config = None;

        let mut args = args.into_iter().map(Into::into);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => return Err(ConfigError::Help(USAGE.to_string())),
                "--api-url" => {
                    cli_url = Some(args.next().ok_or(ConfigError::MissingValue("--api-url"))?);
                }
                "--token" => {
                    cli_token = Some(args.next().ok_or(ConfigError::MissingValue("--token"))?);
                }
                "--config" => {
                    cli_config = Some(args.next().ok_or(ConfigError::MissingValue("--config"))?);
                }
                other => return Err(ConfigError::UnknownArg(other.to_string())),
            }
        }

        let (config_path, explicit) =
            match non_empty(cli_config).or_else(|| non_empty(env("HOF_TUI_CONFIG"))) {
                Some(path) => (Some(expand_home(&path, env)), true),
                None => (default_config_path(env), false),
            };
        let file = match &config_path {
            Some(path) => load_file(path, explicit)?,
            None => None,
        };

        let api_url = non_empty(cli_url)
            .or_else(|| non_empty(env("HOF_API_URL")))
            .or_else(|| file.as_ref().and_then(|f| f.api_url.clone()));

        let token = match non_empty(cli_token).or_else(|| non_empty(env("HOF_API_TOKEN"))) {
            Some(token) => Some(token),
            None => match &file {
                Some(LoadedFile {
                    path,
                    token: Some(source),
                    ..
                }) => Some(source.resolve(path, env)?),
                _ => None,
            },
        };

        Ok(Self {
            api_url: normalize_url(api_url.as_deref().unwrap_or_default()),
            token,
            save_path: if file.is_none() { config_path } else { None },
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

/// Treat unset, empty, and whitespace-only values alike.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Expand a leading `~/` to `$HOME`; anything else is taken literally.
fn expand_home(path: &str, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match (path.strip_prefix("~/"), non_empty(env("HOME"))) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// `$XDG_CONFIG_HOME/hofvarpnir/tui.toml`, falling back to
/// `$HOME/.config/hofvarpnir/tui.toml`. A relative `XDG_CONFIG_HOME` is
/// ignored, as the XDG base directory spec requires.
#[must_use]
pub fn default_config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = non_empty(env("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| non_empty(env("HOME")).map(|home| Path::new(&home).join(".config")))?;
    Some(base.join("hofvarpnir").join("tui.toml"))
}

/// Read and validate the config file. A missing file is fine at the default
/// location and an error when the user named the path.
fn load_file(path: &Path, explicit: bool) -> Result<Option<LoadedFile>, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !explicit => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::FileNotFound(path.to_path_buf()));
        }
        Err(source) => {
            return Err(ConfigError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: FileConfig = toml::from_str(&text).map_err(|e| ConfigError::ParseFile {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let token = match (file.token, file.token_file, file.token_command) {
        (None, None, None) => None,
        (Some(token), None, None) => {
            ensure_private(path)?;
            Some(TokenSource::Inline(token))
        }
        (None, Some(token_file), None) => Some(TokenSource::File(token_file)),
        (None, None, Some(command)) => Some(TokenSource::Command(command)),
        _ => return Err(ConfigError::ConflictingTokenSources(path.to_path_buf())),
    };

    Ok(Some(LoadedFile {
        path: path.to_path_buf(),
        api_url: file.api_url,
        token,
    }))
}

/// Refuse an inline token in a file other users can read, like ssh does for
/// private keys. Follows symlinks: the target's mode is what matters.
// `0o077` is the group+other permission mask, as ssh checks it;
// clippy's `trailing_zeros() >= 6` is equivalent but hides that intent.
#[allow(clippy::verbose_bit_mask)]
fn ensure_private(path: &Path) -> Result<(), ConfigError> {
    let metadata = fs::metadata(path).map_err(|source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(ConfigError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        })
    }
}

impl TokenSource {
    /// Produce the token, trimmed. `config_path` is only used in messages.
    fn resolve(
        &self,
        config_path: &Path,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<String, ConfigError> {
        let (raw, origin) = match self {
            Self::Inline(token) => (
                token.clone(),
                format!("`token` in {}", config_path.display()),
            ),
            Self::File(token_file) => {
                let path = expand_home(token_file, env);
                let raw = fs::read_to_string(&path).map_err(|source| ConfigError::TokenFile {
                    path: path.clone(),
                    source,
                })?;
                (raw, format!("token_file {}", path.display()))
            }
            Self::Command(command) => (
                run_token_command(command)?,
                format!("output of token_command `{command}`"),
            ),
        };
        let token = raw.trim();
        if token.is_empty() {
            Err(ConfigError::EmptyToken(origin))
        } else {
            Ok(token.to_string())
        }
    }
}

/// Run `command` through `sh -c` (so pipes and quoting work as typed) and
/// return its stdout. stdin and stderr stay on the terminal, which the TUI
/// has not taken over yet: a pinentry or `sudo` prompt still works, and a
/// failing command explains itself.
fn run_token_command(command: &str) -> Result<String, ConfigError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .map_err(|source| ConfigError::TokenCommandSpawn {
            command: command.to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(ConfigError::TokenCommandFailed {
            command: command.to_string(),
            status: output.status,
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|_| ConfigError::TokenCommandNotUtf8(command.to_string()))
}

/// Write `config` to `path` as a config file only the owner can read,
/// creating missing parent directories with mode 700.
///
/// The file is written next to `path` and renamed into place, so an
/// interrupted save never leaves a half-written config behind.
///
/// # Errors
///
/// Any I/O error, e.g. a read-only config directory.
pub fn save(path: &Path, config: &Config) -> io::Result<()> {
    #[derive(Serialize)]
    struct Saved<'a> {
        api_url: &'a str,
        token: &'a str,
    }

    let body = toml::to_string(&Saved {
        api_url: &config.api_url,
        token: &config.token,
    })
    .map_err(io::Error::other)?;

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let result = write_private(&tmp, &body).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        drop(fs::remove_file(&tmp));
    }
    result
}

/// Create `path` fresh with mode 600 and write the header plus `body`.
fn write_private(path: &Path, body: &str) -> io::Result<()> {
    // `mode` only applies on creation; a leftover from an earlier crash could
    // carry other permissions, so start over instead of truncating it.
    if let Err(e) = fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(e);
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(SAVED_HEADER.as_bytes())?;
    file.write_all(body.as_bytes())?;
    file.sync_all()
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

    /// Resolve configuration from process args (excluding argv[0]), env, and
    /// the config file.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` on unknown args, missing values, a broken config
    /// file, or a missing/malformed token.
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
    //! Failure modes guarded here, written down before the loader:
    //!
    //! 1. No file at the default path must not be an error (first run).
    //! 2. A file named via `--config` / `HOF_TUI_CONFIG` that is missing must
    //!    be an error, not a silent fallback to the setup screen.
    //! 3. A typo'd key (`token_cmd`) must be rejected, not ignored.
    //! 4. Two token sources at once are ambiguous and must be rejected.
    //! 5. An inline `token` in a group/world-readable file must be refused;
    //!    the same file without an inline secret (home-manager store symlink,
    //!    mode 444) must load.
    //! 6. A failing `token_command` must surface, not fall through to setup.
    //! 7. Empty token output (command or file) must be an error.
    //! 8. A trailing newline from `pass` / `cat` must be trimmed.
    //! 9. A missing `token_file` must name the file; `~/` must expand.
    //! 10. `--token` / `HOF_API_TOKEN` must skip `token_command` entirely (it
    //!     may prompt for a passphrase or have side effects).
    //! 11. URL precedence: CLI > env > file > default; empty env is unset.
    //! 12. A relative `XDG_CONFIG_HOME` must be ignored (XDG spec).
    //! 13. Saving must create mode 600 (dir 700), leave no temp file, and
    //!     round-trip through the loader.
    //! 14. Saving into a read-only directory must fail without debris.
    //! 15. An existing config file must never be offered for overwriting.
    //! 16. A token from the file still goes through `hof_sk_` validation.

    use super::*;

    /// Env lookup over a fixed list, so tests never see the developer's real
    /// `HOME`, `XDG_CONFIG_HOME`, or `HOF_*` variables (or config file).
    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Write `<xdg>/hofvarpnir/tui.toml` with the given mode.
    fn write_config(xdg: &Path, body: &str, mode: u32) -> PathBuf {
        let path = xdg.join("hofvarpnir").join("tui.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn resolve_in(
        xdg: &Path,
        cli: &[&str],
        extra_env: &[(&str, &str)],
    ) -> Result<PartialConfig, ConfigError> {
        let xdg = xdg.to_str().unwrap();
        let mut vars = vec![("XDG_CONFIG_HOME", xdg)];
        vars.extend_from_slice(extra_env);
        PartialConfig::resolve(args(cli), &env_of(&vars))
    }

    #[test]
    fn rejects_missing_token() {
        let err = PartialConfig::resolve(Vec::<String>::new(), &env_of(&[]))
            .unwrap()
            .into_config()
            .unwrap_err();
        assert!(matches!(err, ConfigError::MissingToken));
    }

    #[test]
    fn rejects_non_prefixed_token() {
        let err = PartialConfig::resolve(args(&["--token", "nope"]), &env_of(&[]))
            .unwrap()
            .into_config()
            .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken));
    }

    #[test]
    fn strips_trailing_slash_from_url() {
        let cfg = PartialConfig::resolve(
            args(&["--api-url", "http://example:3000/", "--token", "hof_sk_abc"]),
            &env_of(&[]),
        )
        .unwrap()
        .into_config()
        .unwrap();
        assert_eq!(cfg.api_url, "http://example:3000");
        assert_eq!(cfg.token, "hof_sk_abc");
    }

    #[test]
    fn help_is_not_an_error_with_side_effects() {
        let err = PartialConfig::resolve(args(&["--help"]), &env_of(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::Help(_)));
    }

    #[test]
    fn missing_default_file_offers_save_path() {
        let dir = tempfile::tempdir().unwrap();
        let partial = resolve_in(dir.path(), &[], &[]).unwrap();
        assert_eq!(partial.token, None);
        assert_eq!(partial.api_url, DEFAULT_API_URL);
        assert_eq!(
            partial.save_path,
            Some(dir.path().join("hofvarpnir").join("tui.toml"))
        );
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        let missing = missing.to_str().unwrap();

        let err = resolve_in(dir.path(), &["--config", missing], &[]).unwrap_err();
        assert!(matches!(err, ConfigError::FileNotFound(_)), "{err}");

        let err = resolve_in(dir.path(), &[], &[("HOF_TUI_CONFIG", missing)]).unwrap_err();
        assert!(matches!(err, ConfigError::FileNotFound(_)), "{err}");
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token_cmd = \"pass show hof\"\n", 0o600);
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(matches!(err, ConfigError::ParseFile { .. }), "{err}");
    }

    #[test]
    fn two_token_sources_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "token = \"hof_sk_a\"\ntoken_command = \"printf hof_sk_b\"\n",
            0o600,
        );
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(
            matches!(err, ConfigError::ConflictingTokenSources(_)),
            "{err}"
        );
    }

    #[test]
    fn inline_token_in_readable_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token = \"hof_sk_secret\"\n", 0o644);
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(
            matches!(err, ConfigError::InsecurePermissions { mode: 0o644, .. }),
            "{err}"
        );
        // The message must not leak the token it refused to use.
        assert!(!err.to_string().contains("hof_sk_secret"));
    }

    #[test]
    fn readonly_store_symlink_without_inline_token_loads() {
        // home-manager: ~/.config/hofvarpnir/tui.toml -> /nix/store/...-tui.toml (0444)
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store-tui.toml");
        fs::write(&store, "token_command = \"printf hof_sk_cmd\"\n").unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o444)).unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("hofvarpnir")).unwrap();
        std::os::unix::fs::symlink(&store, xdg.join("hofvarpnir").join("tui.toml")).unwrap();

        let partial = resolve_in(&xdg, &[], &[]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_cmd"));
        assert_eq!(partial.save_path, None);
    }

    #[test]
    fn failing_token_command_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token_command = \"exit 3\"\n", 0o444);
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(
            matches!(err, ConfigError::TokenCommandFailed { .. }),
            "{err}"
        );
    }

    #[test]
    fn empty_token_command_output_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token_command = \"printf '\\\\n'\"\n", 0o444);
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyToken(_)), "{err}");
    }

    #[test]
    fn token_command_output_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "token_command = \"printf 'hof_sk_abc\\\\n' | cat\"\n",
            0o444,
        );
        let partial = resolve_in(dir.path(), &[], &[]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_abc"));
    }

    #[test]
    fn token_file_expands_home_and_trims() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join("secrets")).unwrap();
        fs::write(home.join("secrets").join("hof"), "hof_sk_from_file\n").unwrap();
        write_config(dir.path(), "token_file = \"~/secrets/hof\"\n", 0o444);

        let partial = resolve_in(dir.path(), &[], &[("HOME", home.to_str().unwrap())]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_from_file"));
    }

    #[test]
    fn missing_token_file_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "token_file = \"/nonexistent/agenix/hof\"\n",
            0o444,
        );
        let err = resolve_in(dir.path(), &[], &[]).unwrap_err();
        assert!(matches!(err, ConfigError::TokenFile { .. }), "{err}");
        assert!(err.to_string().contains("/nonexistent/agenix/hof"));
    }

    #[test]
    fn direct_token_skips_token_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        write_config(
            dir.path(),
            &format!(
                "token_command = \"touch {} && printf hof_sk_file\"\n",
                marker.display()
            ),
            0o444,
        );

        let partial = resolve_in(dir.path(), &["--token", "hof_sk_cli"], &[]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_cli"));
        let partial = resolve_in(dir.path(), &[], &[("HOF_API_TOKEN", "hof_sk_env")]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_env"));
        assert!(!marker.exists(), "token_command ran despite a direct token");

        // Control: without a direct token the command does run.
        let partial = resolve_in(dir.path(), &[], &[]).unwrap();
        assert_eq!(partial.token.as_deref(), Some("hof_sk_file"));
        assert!(marker.exists());
    }

    #[test]
    fn url_precedence_cli_env_file_default() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "api_url = \"https://file.example/\"\n", 0o444);

        let from_file = resolve_in(dir.path(), &[], &[("HOF_API_URL", "  ")]).unwrap();
        assert_eq!(from_file.api_url, "https://file.example");

        let from_env =
            resolve_in(dir.path(), &[], &[("HOF_API_URL", "https://env.example")]).unwrap();
        assert_eq!(from_env.api_url, "https://env.example");

        let from_cli = resolve_in(
            dir.path(),
            &["--api-url", "https://cli.example"],
            &[("HOF_API_URL", "https://env.example")],
        )
        .unwrap();
        assert_eq!(from_cli.api_url, "https://cli.example");
    }

    #[test]
    fn relative_xdg_config_home_is_ignored() {
        let env = env_of(&[("XDG_CONFIG_HOME", "relative/cfg"), ("HOME", "/home/u")]);
        assert_eq!(
            default_config_path(&env),
            Some(PathBuf::from("/home/u/.config/hofvarpnir/tui.toml"))
        );
    }

    #[test]
    fn save_is_private_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let partial = resolve_in(dir.path(), &[], &[]).unwrap();
        let path = partial.save_path.unwrap();
        // A token with characters TOML must escape still round-trips.
        let config = Config::new("https://hof.example", "hof_sk_a\"b\\c").unwrap();

        save(&path, &config).unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert!(!path.with_extension("toml.tmp").exists());

        let reloaded = resolve_in(dir.path(), &[], &[]).unwrap();
        assert_eq!(
            reloaded.save_path, None,
            "existing file offered for overwrite"
        );
        assert_eq!(reloaded.into_config().unwrap(), config);
    }

    #[test]
    fn save_into_readonly_dir_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_dir = dir.path().join("hofvarpnir");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::set_permissions(&cfg_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let path = cfg_dir.join("tui.toml");

        let result = save(&path, &Config::new("http://h", "hof_sk_x").unwrap());

        fs::set_permissions(&cfg_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_dir(&cfg_dir).unwrap().count(), 0);
    }

    #[test]
    fn file_token_is_still_validated() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token_command = \"printf not-a-key\"\n", 0o444);
        let err = resolve_in(dir.path(), &[], &[])
            .unwrap()
            .into_config()
            .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidToken));
    }
}
