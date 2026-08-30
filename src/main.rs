use clap::Parser;

fn main() -> std::process::ExitCode {
    netband::run(netband::cli::Cli::parse())
}
