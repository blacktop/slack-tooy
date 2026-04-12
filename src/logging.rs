use color_eyre::eyre::Result;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init(debug: bool) -> Result<()> {
    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("slack-tooy")
        .join("logs");

    std::fs::create_dir_all(&log_dir)?;

    let log_file = std::fs::File::create(log_dir.join("app.log"))?;

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(log_file)
                .with_ansi(false)
                .with_target(true),
        )
        .init();

    Ok(())
}
