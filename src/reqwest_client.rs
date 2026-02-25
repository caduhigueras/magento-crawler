use reqwest::Client;

use crate::configuration::Settings;

pub fn get_client() -> Client {
    Client::builder()
        .danger_accept_invalid_certs(true)
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
