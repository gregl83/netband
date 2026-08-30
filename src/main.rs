use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    netband::run(netband::cli::Cli::parse()).await
}
