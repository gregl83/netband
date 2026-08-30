use crate::cli::Verbosity;

pub fn init(verbosity: Verbosity) {
    let level = match verbosity {
        Verbosity::Error => tracing::Level::ERROR,
        Verbosity::Warn => tracing::Level::WARN,
        Verbosity::Info => tracing::Level::INFO,
        Verbosity::Debug => tracing::Level::DEBUG,
        Verbosity::Trace => tracing::Level::TRACE,
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .try_init();
}
