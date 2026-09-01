use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const EXIT_INTERNAL: u8 = 5;

fn main() -> ExitCode {
    // Tokio stdout performs writes on blocking worker threads. Owning the runtime instead
    // of using #[tokio::main] lets Netband bound runtime teardown if a downstream stdout
    // reader stops consuming output.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("internal error: cannot start async runtime: {error}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };

    // Run the command to completion first, including the console worker's bounded drain.
    let exit_code = runtime.block_on(netband::run(netband::cli::Cli::parse()));

    // A blocked stdout worker cannot be canceled once its write has started. Limit how
    // long runtime shutdown waits for it so console backpressure cannot hold the process
    // open after durable state and measurement output have finished shutting down.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    exit_code
}
