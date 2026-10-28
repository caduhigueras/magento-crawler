use crate::{
    clickhouse_client::ClickHouseSender,
    configuration::Settings,
    csv_writer::{CsvRow, CsvSender},
    telemetry::{ClickHouseLog, LogResponse, log_response},
};
use chrono::Utc;
use reqwest::header::{AGE, HeaderMap};
use reqwest::{Client, Error, StatusCode};
use std::time::Instant;

pub struct Stats {
    pub requests: usize,
    pub failed: usize,
    pub minutes: f64,
}

pub enum FollowUpAction {
    Sleep,
    Continue,
}

/// Shared by every job of a file. Jobs borrow it, so nothing is cloned per request.
pub struct CrawlParams<'a> {
    reqwest_client: Client,
    clickhouse_tx: Option<ClickHouseSender>,
    crawl_id: String,
    config: &'a Settings,
    pub csv_tx: CsvSender,
}

impl<'a> CrawlParams<'a> {
    pub fn new(
        reqwest_client: Client,
        clickhouse_tx: Option<ClickHouseSender>,
        crawl_id: String,
        config: &'a Settings,
        csv_tx: CsvSender,
    ) -> Self {
        CrawlParams {
            reqwest_client,
            clickhouse_tx,
            crawl_id,
            config,
            csv_tx,
        }
    }

    pub fn get_reqwest_client(&self) -> &Client {
        &self.reqwest_client
    }

    pub fn get_clickhouse_tx(&self) -> Option<&ClickHouseSender> {
        self.clickhouse_tx.as_ref()
    }

    pub fn get_crawl_id(&self) -> &str {
        &self.crawl_id
    }

    pub fn get_config(&self) -> &Settings {
        self.config
    }
}

pub fn prepare_url_for_crawl_job<'a>(
    urls: Vec<String>,
    cookies: &'a [String],
) -> impl Iterator<Item = (String, String)> + 'a {
    urls.into_iter().flat_map(move |url| {
        cookies
            .iter()
            .cloned()
            .map(move |cookie| (url.clone(), cookie))
    })
}

/// With the cache warmer secret, Varnish answers X-Cache: HIT / HIT-STALE / MISS (see custom.vcl).
/// Without it, fall back to Age, which is above 0 when the page was served from cache.
fn is_cached(headers: &HeaderMap, uses_cache_warmer_secret: bool) -> bool {
    if uses_cache_warmer_secret {
        headers
            .get("x-cache")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().to_ascii_uppercase().starts_with("HIT"))
    } else {
        headers
            .get(AGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .is_some_and(|age| age > 0)
    }
}

pub async fn crawl_page(
    crawl_params: &CrawlParams<'_>,
    cookie: &str,
    url: &str,
) -> Result<FollowUpAction, Error> {
    let config = crawl_params.get_config();
    let req_client = crawl_params.get_reqwest_client();

    //---------- Format the request
    let req = if cookie.is_empty() {
        req_client.get(url)
    } else {
        req_client.get(url).header(
            reqwest::header::COOKIE,
            format!("X-Magento-Vary={}", cookie),
        )
    };

    //---------- Send the request and start timer as late as possible
    let start = Instant::now();
    let res = match req.send().await {
        Ok(res) => res,
        Err(e) => {
            //---------- Track transport errors too (timeouts, refused connections, DNS, TLS)
            if config.application.save_errors && cookie.is_empty() {
                let reason = if e.is_timeout() {
                    "Timeout"
                } else if e.is_connect() {
                    "Connection error"
                } else {
                    "Request error"
                };

                let _ = crawl_params
                    .csv_tx
                    .send(CsvRow {
                        url: url.to_string(),
                        status: reason.to_string(),
                    })
                    .await;
            }

            return Err(e);
        }
    };

    //---------- Format log response params
    let duration = start.elapsed();
    let msecs = duration.as_millis();
    let status = res.status();
    let cached = is_cached(
        res.headers(),
        config.application.get_cache_warmer_secret().is_some(),
    );

    //---------- Create response struct and print it
    let response = LogResponse::new(status, url.to_string(), msecs, cached, cookie.to_string());

    if config.telemetry.enable_logging {
        log_response(response, config.telemetry.simplified_logging);
    }

    //---------- Hand the row to the ClickHouse writer, which inserts in batches
    if let Some(clickhouse_tx) = crawl_params.get_clickhouse_tx() {
        let log_data = ClickHouseLog::new(
            Utc::now(),
            crawl_params.get_crawl_id().to_string(),
            status.as_u16(),
            u8::from(cached),
            url.to_string(),
            msecs as u32,
            cookie.to_string(),
        );

        let _ = clickhouse_tx.send(log_data).await;
    }

    //---------- Track errors in a different file
    let error_statuses = [
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::NOT_FOUND,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
    ];

    //---------- Only track for empty cookie, to avoid duplicating errors
    if config.application.save_errors && cookie.is_empty() && error_statuses.contains(&status) {
        let _ = crawl_params
            .csv_tx
            .send(CsvRow {
                url: url.to_string(),
                status: status.to_string(),
            })
            .await;
    }

    //---------- If server is overwhelmed, set sleep
    if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(FollowUpAction::Sleep);
    }

    Ok(FollowUpAction::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn x_cache_is_used_when_secret_is_configured() {
        assert!(is_cached(&headers(&[("x-cache", "HIT")]), true));
        assert!(is_cached(&headers(&[("x-cache", "HIT-STALE")]), true));
        assert!(is_cached(&headers(&[("x-cache", "hit")]), true));
        assert!(!is_cached(&headers(&[("x-cache", "MISS")]), true));
        assert!(!is_cached(&headers(&[]), true));
        //---------- Age is ignored while the secret is configured
        assert!(!is_cached(&headers(&[("age", "120")]), true));
    }

    #[test]
    fn age_is_used_without_secret() {
        assert!(is_cached(&headers(&[("age", "120")]), false));
        assert!(!is_cached(&headers(&[("age", "0")]), false));
        assert!(!is_cached(&headers(&[("age", "abc")]), false));
        assert!(!is_cached(&headers(&[]), false));
        //---------- X-Cache is ignored without the secret
        assert!(!is_cached(&headers(&[("x-cache", "HIT")]), false));
    }
}
