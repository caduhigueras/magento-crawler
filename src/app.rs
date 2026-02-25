use crate::crawler::CrawlParams;
use crate::crawler::FollowUpAction;
use crate::crawler::crawl_page;
use crate::crawler::prepare_url_for_crawl_job;
use crate::file_manager::get_files_from_dir;
use crate::file_manager::parse_csv_as_urls;
use crate::reqwest_client;
use crate::{clickhouse_client, configuration::Settings};
use chrono::{DateTime, Local};
use fs::rename;
use futures::StreamExt;
use futures::stream;
use std::fs;
use std::fs::{create_dir, exists};
use std::time::{Instant, SystemTime};
use tokio::time::{Duration, sleep};
use tracing::{error, warn};

pub async fn run(config: Settings) {
    let ch_client = clickhouse_client::get(&config);
    let req_client = reqwest_client::get_client();
    let cookies = reqwest_client::prepare_cookies(&config);

    //---------- Read files sorted by oldest first. Return if empty
    let files = get_files_from_dir(&config.application.input_dir).unwrap();
    if files.is_empty() {
        println!("Directory is empty. Nothing to do.");
        return;
    }

    //---------- Iterate files
    for file in files {
        let system_now = SystemTime::now();
        let datetime: DateTime<Local> = system_now.into();
        let start_formatted = datetime.format("%Y%m%d%H%M").to_string();
        let start = Instant::now();

        let path = format!("{}/{}", config.application.input_dir, &file);
        let urls = parse_csv_as_urls(&path);

        if urls.is_empty() {
            println!("No URLs in {}", &path);
            continue;
        }

        //---------- Build (url, cookie) jobs
        let jobs = prepare_url_for_crawl_job(urls, &cookies); // HACK:

        let results = stream::iter(jobs.map(|(url, cookie)| {
            let crawl_params = CrawlParams::new(
                req_client.clone(),
                ch_client.clone(),
                file.clone(),
                start_formatted.clone(),
                config.clone(),
            );

            async move {
                match crawl_page(crawl_params, &cookie, &url).await {
                    Ok(FollowUpAction::Continue) => Ok::<_, reqwest::Error>(()),
                    Ok(FollowUpAction::Sleep) => {
                        warn!("Waiting 5 min. before resuming (triggered by {})", url); // TODO: Replace sleep time with settings
                        sleep(Duration::from_secs(300)).await;
                        Ok(())
                    }
                    Err(e) => {
                        error!("Error crawling page {}: {}", url, e);
                        Ok(())
                    }
                }
            }
        }))
        .buffer_unordered(config.application.concurrency as usize) // <= cap in-flight requests
        .collect::<Vec<_>>()
        .await;

        let len = results.len();
        let duration = start.elapsed();
        let secs = duration.as_secs() as f64;
        let minutes = secs / 60.00;

        println!("Job executed. File: {}", path);
        println!("{} in requests {:.2?} minutes", len, minutes);

        let history_dir_path = format!("{}/.history", &config.application.input_dir);
        let history_output_dir_exists = exists(&history_dir_path).unwrap();

        if !history_output_dir_exists {
            create_dir(&history_dir_path).expect("Failed to create .history dir");
        }
        let history_filename = format!("{}_{}", start_formatted, file);

        // let input_path
        let output_path = format!(
            "{}/.history/{}",
            &config.application.input_dir, &history_filename
        );
        rename(&path, &output_path).expect("Failed to move file");
    }
}
