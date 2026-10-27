use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::ExposeSecret;

use crate::configuration::Settings;

/// Request header checked by custom.vcl before adding X-Cache to the response.
pub const CACHE_WARMER_HEADER: &str = "x-cache-warmer";

pub fn get_client(config: &Settings) -> Client {
    //---------- gzip() sends Accept-Encoding: gzip, matching what browsers send to Varnish
    let mut builder = Client::builder()
        .danger_accept_invalid_certs(true)
        .gzip(true)
        .user_agent(config.application.get_user_agent());

    //---------- Send the secret on every request so Varnish answers with X-Cache: HIT / MISS
    if let Some(secret) = config.application.get_cache_warmer_secret() {
        let mut value = HeaderValue::from_str(secret.expose_secret().trim())
            .expect("cache_warmer_secret contains characters not allowed in a header");
        value.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert(CACHE_WARMER_HEADER, value);
        builder = builder.default_headers(headers);
    }

    builder.build().unwrap()
}

pub fn prepare_cookies(config: &Settings) -> Vec<String> {
    //---------- Always start with the empty cookie, so every URL is also requested without vary
    let mut cookies = vec![String::new()];

    //---------- Trim values and skip empty or repeated entries (e.g. "abc, def," or "abc,,abc")
    for cookie in config.application.cookies.split(',').map(str::trim) {
        if !cookie.is_empty() && !cookies.iter().any(|c| c == cookie) {
            cookies.push(cookie.to_string());
        }
    }

    cookies
}
