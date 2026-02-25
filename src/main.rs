use magento_crawler::app::run;
use magento_crawler::configuration::get_configuration;
use std::fs::exists;
use tracing::Level;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let config = get_configuration().expect("Failed to load settings");

    //---------- If input dir doesn't exist, break execution
    if !exists(&config.application.input_dir).unwrap() {
        panic!(
            "The dir defined in the config doesnt exist. Maker sure you have it created at: {}",
            &config.application.input_dir
        );
    }

    run(config).await;
}
