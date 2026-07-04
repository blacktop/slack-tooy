mod action;
mod app;
mod components;
mod config;
mod download;
mod emoji;
mod event;
mod logging;
mod slack;
mod store;
mod tui;
mod ui;

use clap::Parser;
use color_eyre::eyre::{Result, bail};
use tokio::sync::mpsc;

use crate::app::App;
use crate::config::Config;
use crate::tui::Tui;

#[derive(Parser)]
#[command(name = "slack-tooy")]
#[command(about = "A terminal Slack client with vim-style keybindings")]
#[command(version)]
struct Cli {
    /// Path to config file.
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Slack API token (overrides config file).
    #[arg(short, long)]
    token: Option<String>,

    /// Browser session cookie for xoxc- tokens (the `d` value).
    #[arg(long)]
    cookie: Option<String>,

    /// Enable debug logging.
    #[arg(short, long)]
    debug: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    let mut config = Config::load(cli.config.as_deref())?;
    logging::init(cli.debug)?;

    // Token precedence: CLI flag > config file > env var
    if let Some(token) = cli.token {
        config.slack_token = token;
    }
    if config.slack_token.is_empty()
        && let Ok(token) = std::env::var("SLACK_TOKEN")
    {
        config.slack_token = token;
    }
    // Cookie precedence: CLI flag > config file > env var
    if let Some(cookie) = cli.cookie {
        config.cookie = cookie;
    }
    if config.cookie.is_empty()
        && let Ok(cookie) = std::env::var("SLACK_COOKIE")
    {
        config.cookie = cookie;
    }
    // xoxc- tokens require a cookie
    if config.slack_token.starts_with("xoxc-") && config.cookie.is_empty() {
        bail!(
            "xoxc- tokens require a browser cookie. \
             Set cookie in config.toml, pass --cookie, \
             or set SLACK_COOKIE env var.\n\
             To get it: open browser dev tools on Slack, \
             Application > Cookies > find 'd' cookie value."
        );
    }
    if config.slack_token.is_empty() {
        bail!(
            "No Slack token found. Set it in \
             ~/.config/slack-tooy/config.toml, \
             pass --token, or set SLACK_TOKEN env var."
        );
    }

    tracing::info!("Starting slack-tooy");

    // Query terminal image protocol support before entering raw mode
    let picker = ratatui_image::picker::Picker::from_query_stdio().ok();
    if picker.is_some() {
        tracing::info!("Terminal image protocol detected");
    }

    let store = store::Store::open()?;

    let (action_tx, action_rx) = mpsc::unbounded_channel();

    let mut tui = Tui::new()?;
    let mut app = App::new(config, action_tx, picker, store)?;

    tui.enter()?;
    let result = app.run(&mut tui, action_rx).await;
    app.save_on_quit();
    tui.exit()?;

    result
}
