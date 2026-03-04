use crate::crawler::CrawlParams;
use crate::crawler::FollowUpAction;
use crate::crawler::Stats;
use crate::crawler::crawl_page;
use crate::crawler::prepare_url_for_crawl_job;
use crate::csv_writer::spawn_csv_writer;
use crate::email_sender::send;
use crate::file_manager::check_and_create_csv_errors_dir;
use crate::file_manager::check_and_create_history_folder;
use crate::file_manager::get_files_from_dir;
use crate::file_manager::has_at_least_one_line;
use crate::file_manager::parse_csv_as_urls;
use crate::reqwest_client;
use crate::{clickhouse_client, configuration::Settings};
use chrono::{DateTime, Local};
use fs::rename;
use futures::StreamExt;
use futures::stream;
use std::fs;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime};
use tokio::time::{Duration, sleep};
use tracing::{error, warn};

pub async fn run(config: Settings) {
    let ch_client = clickhouse_client::get(&config);
    let req_client = reqwest_client::get_client();
    let cookies = reqwest_client::prepare_cookies(&config);
    let mut report_files: Vec<(String, String, Stats, bool)> = Vec::new();

    //---------- Read files sorted by oldest first. Return if empty
    let files = get_files_from_dir(&config.application.input_dir).unwrap();
    if files.is_empty() {
        println!("Directory is empty. Nothing to do.");
        return;
    }

    let system_now = SystemTime::now();
    let datetime: DateTime<Local> = system_now.into();
    let start_formatted = datetime.format("%Y%m%d%H%M").to_string();

    let sleep_time_min = Arc::new(AtomicU64::new(2));
    let times_stopped = Arc::new(AtomicU64::new(1));

    //---------- Iterate files
    for file in files {
        let start = Instant::now();
        let pause_gate = Arc::new(RwLock::new(()));

        let csv_errors_dir = check_and_create_csv_errors_dir(&config, &datetime);
        let reports_file_path = format!("{}/{}_errors_{}", &csv_errors_dir, start_formatted, file);
        let (csv_tx, csv_handle) = spawn_csv_writer(&reports_file_path, 1_000);

        let path = format!("{}/{}", config.application.input_dir, &file);
        let urls = parse_csv_as_urls(&path);

        if urls.is_empty() {
            println!("No URLs in {}", &path);
            continue;
        }

        //---------- Build (url, cookie) jobs
        let jobs = prepare_url_for_crawl_job(urls, &cookies);

        let results = stream::iter(jobs.map(|(url, cookie)| {
            let csv_tx = csv_tx.clone();

            let pause_gate = Arc::clone(&pause_gate);
            let sleep_time_min = Arc::clone(&sleep_time_min);
            let times_stopped = Arc::clone(&times_stopped);

            let crawl_params = CrawlParams::new(
                req_client.clone(),
                ch_client.clone(),
                file.clone(),
                start_formatted.clone(),
                config.clone(),
                csv_tx,
            );

            async move {
                //---------- Wait if another task is sleeping (holding the write lock)
                let _read = pause_gate.read().await;

                match crawl_page(crawl_params, &cookie, &url).await {
                    Ok(FollowUpAction::Continue) => Ok::<_, reqwest::Error>(()),
                    Ok(FollowUpAction::Sleep) => {
                        //---------- Drop read to acquire write lock
                        drop(_read);

                        //---------- Acquire exclusive write lock - block all other tasks at their
                        //---------- read().await
                        let _write = pause_gate.write().await;

                        let min = sleep_time_min.load(Ordering::Relaxed);
                        let stopped = times_stopped.load(Ordering::Relaxed);
                        let sleep_time_sec = min * stopped * 60;

                        warn!(
                            "Waiting {} min. before resuming (triggered by {})",
                            min, url
                        );
                        sleep_time_min.fetch_add(1, Ordering::Relaxed);
                        times_stopped.fetch_add(1, Ordering::Relaxed);
                        sleep(Duration::from_secs(sleep_time_sec)).await;

                        //---------- _write gets dropped here and unblocks other streams
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

        let requests = results.len();
        let duration = start.elapsed();
        let secs = duration.as_secs() as f64;
        let minutes = secs / 60.00;

        println!("Job executed. File: {}", path);
        println!("{} in requests {:.2?} minutes", requests, minutes);

        //---------- Move the file to the history folder
        let history_dir_path = check_and_create_history_folder(&config, datetime);
        let history_filename = format!("{}_{}", start_formatted, file);
        let output_path = format!("{}/{}", &history_dir_path, &history_filename);
        rename(&path, &output_path).expect("Failed to move file");

        //---------- Drop the writer so the CSV contents gets written before evaluating lines
        drop(csv_tx);
        if let Err(e) = csv_handle.await {
            eprintln!("CSV writer task panicked: {e}");
        }

        //---------- Check if csv has at least 1 line and push to vec that will be sent as email
        let has_errors = has_at_least_one_line(&reports_file_path);
        report_files.push((
            file,
            reports_file_path,
            Stats { requests, minutes },
            has_errors,
        ));
    }

    //---------- Job is processed. If set, send email with report files
    if config.application.send_email {
        println!("Sending email...");
        send(&config, &report_files);
    }
}
