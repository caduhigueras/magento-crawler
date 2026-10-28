use core::panic;
use secrecy::{ExposeSecret, SecretString};
use serde_aux::field_attributes::deserialize_number_from_string;
use serde_aux::prelude::*;
use std::{fs, path::PathBuf};

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Settings {
    pub application: ApplicationSettings,
    pub clickhouse: ClickHouseSettings,
    pub telemetry: TelemetrySettings,
    pub email: EmailSettings,
    /// Only needed by `magento_crawler serve`
    #[serde(default)]
    pub server: Option<ServerSettings>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub enum Environment {
    LOCAL,
    PRODUCTION,
}

impl Environment {
    pub fn as_str(&self) -> &str {
        match self {
            Environment::LOCAL => "local",
            Environment::PRODUCTION => "production",
        }
    }
}

impl TryFrom<String> for Environment {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::LOCAL),
            "production" => Ok(Self::PRODUCTION),
            other => Err(format!(
                "{} is not one of the supported envs: (local | production)",
                other
            )),
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ApplicationSettings {
    pub input_dir: String,
    pub cookies: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub concurrency: i32,
    pub save_to_clickhouse: bool,
    pub save_errors: bool,
    pub send_email: bool,
    pub reports_server: String,
    pub reports_folder: String,
    pub sleep_timeout_seconds: u32,
    #[serde(deserialize_with = "deserialize_vec_from_string_or_vec")]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub cache_warmer_secret: Option<SecretString>,
    #[serde(default)]
    pub user_agent: Option<String>,
}

impl ApplicationSettings {
    /// The secret sent to Varnish to get the X-Cache header. An empty value counts as not set.
    pub fn get_cache_warmer_secret(&self) -> Option<&SecretString> {
        self.cache_warmer_secret
            .as_ref()
            .filter(|s| !s.expose_secret().trim().is_empty())
    }

    /// The User-Agent sent on every request. An empty value falls back to "magento_crawler/<version>".
    pub fn get_user_agent(&self) -> String {
        self.user_agent
            .as_deref()
            .map(str::trim)
            .filter(|ua| !ua.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("magento_crawler/{}", env!("CARGO_PKG_VERSION")))
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClickHouseSettings {
    pub clickhouse_client: String,
    pub clickhouse_user: String,
    pub clickhouse_pwd: SecretString,
    pub clickhouse_db: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct TelemetrySettings {
    pub enable_logging: bool,
    pub simplified_logging: bool,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct EmailSettings {
    pub send_from: String,
    pub send_to: String,
    pub subject: String,
    #[serde(deserialize_with = "deserialize_vec_from_string_or_vec")]
    pub send_bcc: Vec<String>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ServerSettings {
    #[serde(default = "default_server_host")]
    pub host: String,
    #[serde(
        default = "default_server_port",
        deserialize_with = "deserialize_number_from_string"
    )]
    pub port: u16,
    /// Sent by clients as `Authorization: Bearer <api_token>`
    pub api_token: SecretString,
    /// Limit for the decompressed upload, in MB
    #[serde(
        default = "default_max_upload_mb",
        deserialize_with = "deserialize_number_from_string"
    )]
    pub max_upload_mb: u64,
    /// How many URLs sent to /urls can wait to be crawled before new requests get a 503
    #[serde(
        default = "default_queue_capacity",
        deserialize_with = "deserialize_number_from_string"
    )]
    pub queue_capacity: usize,
}

fn default_server_host() -> String {
    String::from("127.0.0.1")
}

fn default_server_port() -> u16 {
    8080
}

fn default_max_upload_mb() -> u64 {
    100
}

fn default_queue_capacity() -> usize {
    100_000
}

pub fn get_configuration() -> Result<Settings, config::ConfigError> {
    let os_config_dir: PathBuf = dirs::config_dir().expect("Failed to determine config directory.");
    let config_dir = os_config_dir.join("magento_crawler/");
    let config_file = config_dir.join("config.toml");

    let exp_msg = format!("Make sure config file exists at: {:?}", config_file);

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
