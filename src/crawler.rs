use crate::{
    clickhouse_client,
    configuration::Settings,
    telemetry::{ClickHouseLog, LogResponse, log_response},
};
use chrono::Utc;
use clickhouse::Client as ClickHouseClient;
use reqwest::{Client, Error, StatusCode};
use std::time::Instant;

pub enum FollowUpAction {
    Sleep,
    Continue,
}

pub struct CrawlParams {
    reqwest_client: Client,
    clickhouse_client: ClickHouseClient,
    file: String,
    start_formatted: String,
    config: Settings,
}

impl CrawlParams {
    pub fn new(
        reqwest_client: Client,
        clickhouse_client: ClickHouseClient,
        file: String,
        start_formatted: String,
        config: Settings,
    ) -> Self {
        CrawlParams {
            reqwest_client,
            clickhouse_client,
            file,
            start_formatted,
            config,
        }
    }

    pub fn get_reqwest_client(&self) -> &Client {
        &self.reqwest_client
    }

    pub fn get_clickhouse_client(&self) -> &ClickHouseClient {
        &self.clickhouse_client
    }

    pub fn get_file(&self) -> &str {
        &self.file
    }

    pub fn get_start_formatted(&self) -> &str {
        &self.start_formatted
    }

    pub fn get_config(&self) -> &Settings {
        &self.config
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

pub async fn crawl_page(
    crawl_params: CrawlParams,
    cookie: &str,
    url: &str,
) -> Result<FollowUpAction, Error> {
    let start = Instant::now();
    let config = crawl_params.get_config();

    //---------- Actual REQ
    let res = if cookie.is_empty() {
        crawl_params.get_reqwest_client().get(url).send().await?
    } else {
        crawl_params
            .get_reqwest_client()
            .get(url)
            .header(
                reqwest::header::COOKIE,
                format!("X-Magento-Vary={}", cookie),
            )
            .send()
            .await?
    };

    //---------- Format log response params
    let duration = start.elapsed();
    let msecs = duration.as_millis();
    let headers = res.headers();
    let status = res.status();
    let varnish_tags = if let Some(v) = headers.get("x-varnish") {
        v.to_str().unwrap_or("")
    } else {
        ""
    };
    let tags_vec = varnish_tags.split(" ").collect::<Vec<&str>>();
    let cached = tags_vec.len() > 1;

    //---------- Create response struct and print it
    let response = LogResponse::new(
        status,
        url.to_string(),
        msecs,
        cached,
        varnish_tags.to_string(),
        cookie.to_string(),
    );

    if config.telemetry.enable_logging {
        log_response(response, config.telemetry.simplified_logging);
    }

    //---------- Save into ClickHouse
    if config.application.save_to_clickhouse {
        //---------- Append formatted timestamp so we can always see watch crawl individually in grafana
        let formatted_crawl_id = format!(
            "{}_{}",
            crawl_params.get_file(),
            crawl_params.get_start_formatted()
        );

        let log_data = ClickHouseLog::new(
            Utc::now(),
            formatted_crawl_id,
            status.as_u16(),
            u8::from(cached),
            url.to_string(),
            msecs as u32,
            varnish_tags.to_string(),
            cookie.to_string(),
        );

        clickhouse_client::save(log_data, crawl_params.get_clickhouse_client()).await;
    }

    //---------- If server is overwhelmed, set sleep
    // if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
    if status == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(FollowUpAction::Sleep);
    }

    Ok(FollowUpAction::Continue)
}
