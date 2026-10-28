use clickhouse::{Client, inserter::Inserter};
use secrecy::ExposeSecret;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::error;

use crate::{configuration::Settings, telemetry::ClickHouseLog};

/// Sender type you can clone and pass into your crawl tasks.
pub type ClickHouseSender = mpsc::Sender<ClickHouseLog>;

pub fn get(config: &Settings) -> Client {
    //---------- Clickhouse client
    Client::default()
        .with_url(&config.clickhouse.clickhouse_client)
        .with_user(&config.clickhouse.clickhouse_user)
        .with_password(config.clickhouse.clickhouse_pwd.expose_secret())
        .with_database(&config.clickhouse.clickhouse_db)
}

/// Spawns a single task that receives log rows and inserts them in batches,
/// instead of running one INSERT per request.
pub fn spawn_writer(
    client: Client,
    channel_capacity: usize,
) -> (ClickHouseSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<ClickHouseLog>(channel_capacity);

    let handle = tokio::spawn(async move {
        let mut inserter = new_inserter(&client);

        while let Some(row) = rx.recv().await {
            let result = match inserter.write(&row) {
                Ok(()) => inserter.commit().await.map(|_| ()),
                Err(e) => Err(e),
            };

            //---------- Don't stop the crawl: log, drop the failed batch and start a new inserter
            if let Err(e) = result {
                error!(
                    "Failed to save crawl logs to ClickHouse, batch dropped: {}",
                    e
                );
                inserter = new_inserter(&client);
            }
        }

        //---------- Channel closed: flush whatever is still pending
        if let Err(e) = inserter.end().await {
            error!("Failed to save last crawl logs batch to ClickHouse: {}", e);
        }
    });

    (tx, handle)
}

fn new_inserter(client: &Client) -> Inserter<ClickHouseLog> {
    client
        .inserter("crawl_logs")
        .expect("Failed to create ClickHouse inserter")
        .with_max_rows(10_000)
        .with_period(Some(Duration::from_secs(5)))
}
