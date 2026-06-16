//! Agent configuration, read from the environment.
//!
//! Every value is configurable; only the bridge token is required. Defaults
//! target the common case: a local Ollama reached from inside a container.

use std::time::Duration;

/// Environment variable holding the bridge access token (required).
const ENV_TOKEN: &str = "PENFREELY_BRIDGE_TOKEN";
/// Environment variable for the backend websocket URL.
const ENV_BACKEND_WS_URL: &str = "PENFREELY_BACKEND_WS_URL";
/// Environment variable for the local Ollama base URL.
const ENV_OLLAMA_URL: &str = "PENFREELY_OLLAMA_URL";
/// Environment variable for the initial reconnect backoff in milliseconds.
const ENV_BACKOFF_INITIAL_MS: &str = "PENFREELY_RECONNECT_INITIAL_MS";
/// Environment variable for the maximum reconnect backoff in milliseconds.
const ENV_BACKOFF_MAX_MS: &str = "PENFREELY_RECONNECT_MAX_MS";

/// Default backend websocket URL (local development backend).
const DEFAULT_BACKEND_WS_URL: &str = "ws://localhost:8080/bridge/connect";
/// Default Ollama URL: the local runtime on the same machine. The native binary
/// reaches it directly; the Docker image overrides this to `host.docker.internal`
/// (macOS/Windows) or runs with host networking (Linux).
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Default initial reconnect backoff.
const DEFAULT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// Default maximum reconnect backoff.
const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Validated agent configuration.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Bridge access token presented as a bearer credential.
    pub token: String,
    /// Backend websocket URL to connect to.
    pub backend_ws_url: String,
    /// Local Ollama base URL (no trailing slash).
    pub ollama_url: String,
    /// Initial delay before the first reconnect attempt.
    pub backoff_initial: Duration,
    /// Upper bound on the reconnect delay.
    pub backoff_max: Duration,
}

/// A reader of environment variables. Abstracted so the parser is testable
/// without touching the process environment.
pub trait EnvSource {
    /// Read a variable, returning `None` if unset or empty.
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads from the real process environment.
pub struct SystemEnv;

impl EnvSource for SystemEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|value| !value.is_empty())
    }
}

/// Why configuration could not be assembled.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The required bridge token was not set.
    #[error("{0} is required (the bridge token from the service)")]
    MissingToken(&'static str),
    /// A duration variable was not a valid non-negative integer.
    #[error("{key} must be a whole number of milliseconds")]
    InvalidDuration {
        /// The offending variable name.
        key: &'static str,
    },
}

impl AgentConfig {
    /// Build the configuration from an environment source.
    ///
    /// # Errors
    /// Returns [`ConfigError::MissingToken`] if the token is unset, or
    /// [`ConfigError::InvalidDuration`] if a backoff value is not a number.
    pub fn from_env(env: &impl EnvSource) -> Result<Self, ConfigError> {
        let token = env
            .get(ENV_TOKEN)
            .ok_or(ConfigError::MissingToken(ENV_TOKEN))?;
        let backend_ws_url = env
            .get(ENV_BACKEND_WS_URL)
            .unwrap_or_else(|| DEFAULT_BACKEND_WS_URL.to_owned());
        let ollama_url = env
            .get(ENV_OLLAMA_URL)
            .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let backoff_initial = duration_from(env, ENV_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_INITIAL)?;
        let backoff_max = duration_from(env, ENV_BACKOFF_MAX_MS, DEFAULT_BACKOFF_MAX)?;

        Ok(Self {
            token,
            backend_ws_url,
            ollama_url,
            backoff_initial,
            backoff_max,
        })
    }
}

/// Parse a millisecond duration from the environment, or fall back to `default`.
fn duration_from(
    env: &impl EnvSource,
    key: &'static str,
    default: Duration,
) -> Result<Duration, ConfigError> {
    match env.get(key) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| ConfigError::InvalidDuration { key }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct MapEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|value| (*value).to_owned())
        }
    }

    fn env(pairs: &[(&'static str, &'static str)]) -> MapEnv {
        MapEnv(pairs.iter().copied().collect())
    }

    #[test]
    fn requires_a_token() {
        let result = AgentConfig::from_env(&env(&[]));
        assert_eq!(result.unwrap_err(), ConfigError::MissingToken(ENV_TOKEN));
    }

    #[test]
    fn applies_defaults_with_only_a_token() {
        let config = AgentConfig::from_env(&env(&[(ENV_TOKEN, "id.secret")])).unwrap();

        assert_eq!(config.token, "id.secret");
        assert_eq!(config.backend_ws_url, DEFAULT_BACKEND_WS_URL);
        assert_eq!(config.ollama_url, DEFAULT_OLLAMA_URL);
        assert_eq!(config.backoff_initial, DEFAULT_BACKOFF_INITIAL);
        assert_eq!(config.backoff_max, DEFAULT_BACKOFF_MAX);
    }

    #[test]
    fn overrides_values_and_trims_the_ollama_url() {
        let config = AgentConfig::from_env(&env(&[
            (ENV_TOKEN, "t"),
            (ENV_BACKEND_WS_URL, "wss://app.example.com/bridge/connect"),
            (ENV_OLLAMA_URL, "http://127.0.0.1:11434/"),
            (ENV_BACKOFF_INITIAL_MS, "500"),
            (ENV_BACKOFF_MAX_MS, "60000"),
        ]))
        .unwrap();

        assert_eq!(
            config.backend_ws_url,
            "wss://app.example.com/bridge/connect"
        );
        assert_eq!(config.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(config.backoff_initial, Duration::from_millis(500));
        assert_eq!(config.backoff_max, Duration::from_secs(60));
    }

    #[test]
    fn rejects_a_non_numeric_backoff() {
        let result = AgentConfig::from_env(&env(&[(ENV_TOKEN, "t"), (ENV_BACKOFF_MAX_MS, "soon")]));
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidDuration {
                key: ENV_BACKOFF_MAX_MS
            }
        );
    }
}
