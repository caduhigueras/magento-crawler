use clap::{Parser, Subcommand};
use magento_crawler::app::run;
use magento_crawler::configuration::get_configuration;
use magento_crawler::server::serve;
use std::fs::exists;
use tracing::Level;

#[derive(Parser)]
#[command(
    version,
    about = "Warms Magento/Varnish cache for every X-Magento-Vary segment"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl every CSV in input_dir once, then exit (default when no command is given)
    Run,
    /// Keep an HTTP server running: POST /upload saves a CSV for the next `run`,
    /// POST /urls crawls URLs right away
    Serve,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let config = get_configuration().expect("Failed to load settings");

    //---------- If input dir doesn't exist, break execution
    if !exists(&config.application.input_dir).unwrap() {
        panic!(
            "The dir defined in the config doesnt exist. Maker sure you have it created at: {}",
            config.application.input_dir
        );
    }

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(config).await,
        Command::Serve => serve(config).await,
    }
}
