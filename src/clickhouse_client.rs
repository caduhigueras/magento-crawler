use clickhouse::{Client, insert::Insert};
use secrecy::ExposeSecret;

use crate::{configuration::Settings, telemetry::ClickHouseLog};

pub fn get(config: &Settings) -> Client {
    //---------- Clickhouse client
    Client::default()
        .with_url(&config.clickhouse.clickhouse_client)
        .with_user(&config.clickhouse.clickhouse_user)
        .with_password(config.clickhouse.clickhouse_pwd.expose_secret())
        .with_database(&config.clickhouse.clickhouse_db)
}

pub async fn save(log_data: ClickHouseLog, client: &Client) {
    let mut insert: Insert<ClickHouseLog> = client.insert("crawl_logs").unwrap();
    insert
        .write(&log_data)
        .await
        .expect("Error writing to clickhouse");
    insert.end().await.unwrap();
}
