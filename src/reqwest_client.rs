use reqwest::Client;

use crate::configuration::{Environment, Settings};

pub fn get_client(concurrency: usize) -> Client {
    Client::builder()
        .danger_accept_invalid_certs(!is_production())
        .pool_max_idle_per_host(concurrency)
        .build()
        .unwrap()
}

pub fn prepare_cookies(config: &Settings) -> Vec<String> {
    //---------- Convert cookies in str vector
    let mut cookies = config
        .application
        .cookies
        .split(",")
        .map(String::from)
        .collect::<Vec<String>>();

    //---------- Prepend empty cookie
    if !cookies[0].is_empty() {
        cookies.insert(0, String::from(""));
    }

    cookies
}

fn is_production() -> bool {
    let environment: Environment = std::env::var("APP_ENVIRONMENT")
        .unwrap_or_else(|_| "local".into())
        .try_into()
        .expect("Failed to parse APP_ENVIRONMENT.");

    match environment {
        Environment::LOCAL => false,
        Environment::PRODUCTION => true,
    }
}
