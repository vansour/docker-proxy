use std::fs;
use std::path::Path;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use tracing_appender::non_blocking::WorkerGuard;

/// Logger initialization from config
pub fn init_logger(
    log_file_path: &str,
    log_level: &str,
) -> Result<Option<WorkerGuard>, Box<dyn std::error::Error>> {
    // Create log directory if it doesn't exist
    if let Some(parent) = Path::new(log_file_path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    // Parse log level
    let level = parse_log_level(log_level);

    // Create file appender for non-blocking writes
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path)?;

    let (non_blocking, guard) = tracing_appender::non_blocking(file);

    // Create file layer with timestamp (JSON format)
    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_timer(SystemTime)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true);

    // Create console layer with timestamp
    let console_layer = tracing_subscriber::fmt::layer()
        .with_timer(SystemTime)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true);

    // Create env filter
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level.as_str()))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Combine layers and set as global subscriber
    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(console_layer)
        .init();

    Ok(Some(guard))
}

/// Initialize logger with console output only (useful for development)
pub fn init_logger_console(
    log_level: &str,
) -> Result<Option<WorkerGuard>, Box<dyn std::error::Error>> {
    let level = parse_log_level(log_level);

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level.as_str()))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true),
        )
        .init();

    Ok(None)
}

/// Parse log level string to tracing Level
fn parse_log_level(level: &str) -> String {
    match level.to_lowercase().as_str() {
        "debug" => "debug".to_string(),
        "info" => "info".to_string(),
        "warn" => "warn".to_string(),
        "error" => "error".to_string(),
        "trace" => "trace".to_string(),
        _ => "info".to_string(),
    }
}
