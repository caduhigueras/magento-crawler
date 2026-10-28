use crate::clickhouse_client;
use crate::configuration::Settings;
use crate::crawler::{CrawlParams, crawl_job};
use crate::csv_writer::spawn_daily_csv_writer;
use crate::reqwest_client;
use chrono::Local;
use futures::{StreamExt, stream};
use std::future::Future;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::error;

/// Validated URLs waiting to be crawled. Each one is requested once per cookie, like a CSV line.
pub type UrlSender = mpsc::Sender<String>;

/// Spawns one task that crawls URLs as they arrive on the returned sender, with the same
/// concurrency, cookies, sleep on 502/503, ClickHouse and error CSV handling as `run`.
/// It stops when `shutdown` resolves (in-flight requests are dropped) or every sender is dropped.
pub fn spawn_live_crawler(
    config: Settings,
    queue_capacity: usize,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> (UrlSender, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<String>(queue_capacity);

    let handle = tokio::spawn(async move {
        let req_client = reqwest_client::get_client(&config);
        let cookies = reqwest_client::prepare_cookies(&config);
        let sleep_time_in_secs = config.application.sleep_timeout_seconds as u64;
        let concurrency = config.application.concurrency as usize;

        let (ch_tx, ch_handle) = if config.application.save_to_clickhouse {
            let (tx, handle) =
                clickhouse_client::spawn_writer(clickhouse_client::get(&config), 10_000);
            (Some(tx), Some(handle))
        } else {
            (None, None)
        };

        let (csv_tx, csv_handle) =
            spawn_daily_csv_writer(config.application.reports_folder.clone(), "live", 1_000);

        {
            //---------- One crawl_id per server start, so the live crawl shows up on its own in grafana
            let crawl_params = CrawlParams::new(
                req_client,
                ch_tx,
                format!("live_{}", Local::now().format("%Y%m%d%H%M")),
                &config,
                csv_tx,
            );
            let crawl_params = &crawl_params;

            //---------- Build (url, cookie) jobs as URLs come in
            stream::unfold(
                rx,
                |mut rx| async move { rx.recv().await.map(|url| (url, rx)) },
            )
            //---------- Owned cookies per URL: borrowing them here makes the spawned future not Send
            .flat_map(|url| {
                stream::iter(
                    cookies
                        .clone()
                        .into_iter()
                        .map(move |cookie| (url.clone(), cookie)),
                )
            })
            .map(|(url, cookie)| async move {
                crawl_job(crawl_params, &cookie, &url, sleep_time_in_secs).await
            })
            .buffer_unordered(concurrency) // <= cap in-flight requests
            .take_until(shutdown)
            .for_each(|_| async {})
            .await;
            //---------- crawl_params (and its senders) is dropped here
        }

        //---------- Wait for the writers so the last rows are saved
        if let Some(handle) = ch_handle
            && let Err(e) = handle.await
        {
            error!("ClickHouse writer task panicked: {e}");
        }
        if let Err(e) = csv_handle.await {
            error!("CSV writer task panicked: {e}");
        }
    });

    (tx, handle)
}
