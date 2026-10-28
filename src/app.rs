use crate::crawler::CrawlParams;
use crate::crawler::Stats;
use crate::crawler::crawl_job;
use crate::crawler::prepare_url_for_crawl_job;
use crate::csv_writer::spawn_csv_writer;
use crate::email_sender::send;
use crate::file_manager::check_and_create_csv_errors_dir;
use crate::file_manager::check_and_create_history_folder;
use crate::file_manager::get_files_from_dir;
use crate::file_manager::has_at_least_one_line;
use crate::file_manager::normalize_hosts;
use crate::file_manager::parse_csv_as_urls;
use crate::reqwest_client;
use crate::{clickhouse_client, configuration::Settings};
use chrono::{DateTime, Local};
use fs::rename;
use futures::StreamExt;
use futures::stream;
use std::fs;
use std::time::{Instant, SystemTime};

pub async fn run(config: Settings) {
    let req_client = reqwest_client::get_client(&config);
    let cookies = reqwest_client::prepare_cookies(&config);

    //---------- Only URLs pointing to these hosts are crawled. Stop early if none is configured
    let allowed_hosts = normalize_hosts(&config.application.allowed_hosts);
    if allowed_hosts.is_empty() {
        panic!("allowed_hosts is empty in the config, so no URL could be crawled.");
    }
    let mut report_files: Vec<(String, String, Stats, bool)> = Vec::new();

    //---------- Read files sorted by oldest first. Return if empty
    let files = get_files_from_dir(&config.application.input_dir).unwrap();
    if files.is_empty() {
        println!("Directory is empty. Nothing to do.");
        return;
    }

    //---------- One ClickHouse writer for the whole run, inserting in batches
    let (ch_tx, ch_handle) = if config.application.save_to_clickhouse {
        let (tx, handle) = clickhouse_client::spawn_writer(clickhouse_client::get(&config), 10_000);
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let system_now = SystemTime::now();
    let datetime: DateTime<Local> = system_now.into();
    let start_formatted = datetime.format("%Y%m%d%H%M").to_string();
    let sleep_time_in_secs = config.application.sleep_timeout_seconds as u64;

    //---------- Iterate files
    for file in files {
        let start = Instant::now();

        let csv_errors_dir = check_and_create_csv_errors_dir(&config, &datetime);
        let reports_file_path = format!("{}/{}_errors_{}", csv_errors_dir, start_formatted, file);
        let (csv_tx, csv_handle) = spawn_csv_writer(&reports_file_path, 1_000);

        let path = format!("{}/{}", config.application.input_dir, file);
        let urls = parse_csv_as_urls(&path, &allowed_hosts);

        let (requests, failed) = if urls.is_empty() {
            //---------- Still consume the file below, so it isn't picked up again on every run
            println!("No URLs in {}", path);
            drop(csv_tx);
            (0, 0)
        } else {
            //---------- Append formatted timestamp so we can always see watch crawl individually in grafana
            let crawl_params = CrawlParams::new(
                req_client.clone(),
                ch_tx.clone(),
                format!("{}_{}", file, start_formatted),
                &config,
                csv_tx,
            );
            let crawl_params = &crawl_params;

            //---------- Build (url, cookie) jobs
            let jobs = prepare_url_for_crawl_job(urls, &cookies);

            let results = stream::iter(jobs.map(move |(url, cookie)| async move {
                crawl_job(crawl_params, &cookie, &url, sleep_time_in_secs).await
            }))
            .buffer_unordered(config.application.concurrency as usize) // <= cap in-flight requests
            .collect::<Vec<bool>>()
            .await;

            let failed = results.iter().filter(|ok| !**ok).count();
            (results.len(), failed)
            //---------- crawl_params (and its csv_tx) is dropped here
        };

        let minutes = start.elapsed().as_secs_f64() / 60.00;

        println!("Job executed. File: {}", path);
        println!(
            "{} requests in {:.2} minutes ({} failed)",
            requests, minutes, failed
        );

        //---------- Move the file to the history folder
        let history_dir_path = check_and_create_history_folder(&config, datetime);
        let history_filename = format!("{}_{}", start_formatted, file);
        let output_path = format!("{}/{}", history_dir_path, history_filename);
        rename(&path, &output_path).expect("Failed to move file");

        //---------- Wait for the writer so the CSV contents gets written before evaluating lines
        match csv_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("CSV writer failed: {e}"),
            Err(e) => eprintln!("CSV writer task panicked: {e}"),
        }

        //---------- Check if csv has at least 1 line and push to vec that will be sent as email
        let has_errors = has_at_least_one_line(&reports_file_path);
        report_files.push((
            file,
            reports_file_path,
            Stats {
                requests,
                failed,
                minutes,
            },
            has_errors,
        ));
    }

    //---------- Close the channel and wait for the last ClickHouse batch to be flushed
    drop(ch_tx);
    if let Some(handle) = ch_handle
        && let Err(e) = handle.await
    {
        eprintln!("ClickHouse writer task panicked: {e}");
    }

    //---------- Job is processed. If set, send email with report files
    if config.application.send_email {
        println!("Sending email...");
        send(&config, &report_files);
    }
}
