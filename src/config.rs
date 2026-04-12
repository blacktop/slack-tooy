use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub slack_token: String,
    /// Browser session cookie (`d=xoxd-...`), required for `xoxc-` tokens.
    #[serde(default)]
    pub cookie: String,
    /// Sidebar width in 12-column grid units (1-11, default 3).
    pub sidebar_width: u32,
    /// Tick rate in milliseconds.
    pub tick_rate_ms: u64,
    /// How often to poll for new messages (seconds, default 5).
    pub poll_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            slack_token: String::new(),
            cookie: String::new(),
            sidebar_width: 3,
            tick_rate_ms: 250,
            poll_interval_secs: 5,
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = path.map(PathBuf::from).or_else(|| {
            dirs::home_dir().map(|h| h.join(".config").join("slack-tooy").join("config.toml"))
        });

        let Some(config_path) = config_path else {
            return Ok(Self::default());
        };

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&config_path)
            .wrap_err_with(|| format!("Failed to read config: {}", config_path.display()))?;

        let mut config: Config =
            toml::from_str(&contents).wrap_err("Failed to parse config.toml")?;

        config.sidebar_width = config.sidebar_width.clamp(1, 11);
        config.tick_rate_ms = config.tick_rate_ms.max(50);
        config.poll_interval_secs = config.poll_interval_secs.max(1);

        Ok(config)
    }

    pub fn tick_rate(&self) -> Duration {
        Duration::from_millis(self.tick_rate_ms)
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs)
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_config_has_empty_token() {
        let config = Config::default();
        assert!(config.slack_token.is_empty());
        assert_eq!(config.sidebar_width, 3);
        assert_eq!(config.tick_rate_ms, 250);
    }

    #[test]
    fn missing_file_returns_default() {
        let result = Config::load(Some(Path::new("/nonexistent/config.toml")));
        assert!(result.is_ok());
        let config = result.expect("should be ok");
        assert!(config.slack_token.is_empty());
    }

    #[test]
    fn valid_toml_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"slack_token = "xoxp-test-token"
sidebar_width = 4
tick_rate_ms = 100
"#
        )
        .expect("write");

        let config = Config::load(Some(&path)).expect("should parse");
        assert_eq!(config.slack_token, "xoxp-test-token");
        assert_eq!(config.sidebar_width, 4);
        assert_eq!(config.tick_rate_ms, 100);
    }

    #[test]
    fn invalid_toml_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not valid {{{{ toml").expect("write");

        let result = Config::load(Some(&path));
        assert!(result.is_err());
    }

    #[test]
    fn sidebar_width_clamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "sidebar_width = 99").expect("write");

        let config = Config::load(Some(&path)).expect("should parse");
        assert_eq!(config.sidebar_width, 11);
    }

    #[test]
    fn sidebar_width_clamped_low() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "sidebar_width = 0").expect("write");

        let config = Config::load(Some(&path)).expect("should parse");
        assert_eq!(config.sidebar_width, 1);
    }
}
