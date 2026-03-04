use crate::{
    clickhouse_client,
    configuration::Settings,
    csv_writer::CsvRow,
    telemetry::{ClickHouseLog, LogResponse, log_response},
};
use chrono::Utc;
use clickhouse::Client as ClickHouseClient;
use reqwest::{Client, Error, StatusCode};
use std::time::Instant;
use tokio::sync::mpsc::Sender;

pub struct Stats {
    pub requests: usize,
    pub minutes: f64,
}

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
    pub csv_tx: Sender<CsvRow>,
}

impl CrawlParams {
    pub fn new(
        reqwest_client: Client,
        clickhouse_client: ClickHouseClient,
        file: String,
        start_formatted: String,
        config: Settings,
        csv_tx: Sender<CsvRow>,
    ) -> Self {
        CrawlParams {
            reqwest_client,
            clickhouse_client,
            file,
            start_formatted,
            config,
            csv_tx,
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
    let config = crawl_params.get_config();
    let req_client = crawl_params.get_reqwest_client();

    //---------- Format the request
    let req = if cookie.is_empty() {
        req_client.get(url)
    } else {
        req_client
            .get(url)
            .header(
                reqwest::header::COOKIE,
                format!("X-Magento-Vary={}", cookie),
            )
    };

    //---------- Send the request and start timer as late as possible
    let start = Instant::now();
    let res = req.send().await?;

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
