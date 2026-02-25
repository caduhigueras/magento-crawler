use core::panic;
use serde_aux::field_attributes::deserialize_number_from_string;
use std::{fs, path::PathBuf};

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Settings {
    pub application: ApplicationSettings,
    pub clickhouse: ClickHouseSettings,
    pub telemetry: TelemetrySettings,
    pub email: EmailSettings,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ApplicationSettings {
    pub input_dir: String,
    pub cookies: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub concurrency: i32,
    pub save_to_clickhouse: bool,
    pub send_email: bool,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClickHouseSettings {
    pub clickhouse_client: String,
    pub clickhouse_user: String,
    pub clickhouse_pwd: String, // TODO: add secrecy here
    pub clickhouse_db: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct TelemetrySettings {
    pub enable_logging: bool,
    pub simplified_logging: bool,
    pub send_status_email: bool,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct EmailSettings {
    pub send_to: String,
}

pub fn get_configuration() -> Result<Settings, config::ConfigError> {
    let os_config_dir: PathBuf = dirs::config_dir().expect("Failed to determine config directory.");
    let config_dir = os_config_dir.join("magento_crawler/");
    let config_file = config_dir.join("config.toml");

    let exp_msg = format!("Make sure config file exists at: {:?}", &config_file);

    let config_file_exists = fs::exists(&config_file).expect(&exp_msg);
    if !config_file_exists {
        panic!("{}", exp_msg);
    }

    let settings = config::Config::builder()
        .add_source(config::File::from(config_dir.join("config.toml")))
        // Add in settings from env vars with APP_ prefix.
        // E.g. APP_APPLICATION__CONCURRENCY=50 would set Settings.application.concurrency to 50
        .add_source(
            config::Environment::with_prefix("APP")
                .prefix_separator("_")
                .separator("__"),
        )
        .build()?;

    settings.try_deserialize::<Settings>()
}
