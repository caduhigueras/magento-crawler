use chrono::{DateTime, Utc};
use clickhouse::Row;
use reqwest::StatusCode;
use serde::Serialize;
use tracing::info;

#[derive(Debug)]
pub struct LogResponse {
    status: StatusCode,
    cached: bool,
    url: String,
    duration: u128,
    varnish_tags: String,
    cookie: String,
}

impl LogResponse {
    pub fn new(
        status: StatusCode,
        url: String,
        duration: u128,
        cached: bool,
        varnish_tags: String,
        cookie: String,
    ) -> Self {
        LogResponse {
            status,
            url,
            duration,
            cached,
            varnish_tags,
            cookie,
        }
    }

    pub fn get_status(&self) -> StatusCode {
        self.status
    }

    pub fn get_cached(&self) -> bool {
        self.cached
    }

    pub fn get_url(&self) -> &str {
        &self.url
    }

    pub fn get_duration(&self) -> u128 {
        self.duration
    }

    pub fn get_varnish_tags(&self) -> &str {
        &self.varnish_tags
    }

    pub fn get_cookie(&self) -> &str {
        &self.cookie
    }
}

pub fn log_response(response: LogResponse, simplified_logging: bool) {
    if simplified_logging {
        let cached_text = if response.get_cached() {
            String::from("Cached")
        } else {
            String::from("Not Cached")
        };

        info!(
            "{} | {} | {} | {}ms",
            response.get_url(),
            response.get_status(),
            cached_text,
            response.get_duration()
        );
    } else {
        info!(
            "{} - {} | Cached: {} | {}ms | Tags: {} | Cookie: {}",
            response.get_url(),
            response.get_status(),
            response.get_cached(),
            response.get_duration(),
            response.get_varnish_tags(),
            response.get_cookie()
        );
    }
}

#[derive(Row, Serialize)]
pub struct ClickHouseLog {
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    ts: DateTime<Utc>,
    crawl_id: String,
    status: u16,
    cached: u8,
    url: String,
    duration_ms: u32,
    varnish_tags: String,
    cookie_hash: String,
}

impl ClickHouseLog {
    pub fn new(
        ts: DateTime<Utc>,
        crawl_id: String,
        status: u16,
        cached: u8,
        url: String,
        duration_ms: u32,
        varnish_tags: String,
        cookie_hash: String,
    ) -> Self {
        ClickHouseLog {
            ts,
            crawl_id,
            status,
            cached,
            url,
            duration_ms,
            varnish_tags,
            cookie_hash,
        }
    }
}
