use chrono::{DateTime, Local, Utc};
use clap::Parser;
use clickhouse::Client as ChClient;
use clickhouse::Row;
use clickhouse::insert::Insert;
use csv::ReaderBuilder;
use fs::rename;
use futures::StreamExt;
use futures::stream;
use tracing::{info, warn, error, Level};
use reqwest::{Client, Error, StatusCode};
use serde::Serialize;
use std::fs;
use std::fs::{File, create_dir, exists};
use std::time::{Instant, SystemTime};
use tokio::time::{Duration, interval, sleep};

enum FollowUpAction {
    Sleep,
    Continue,
}

#[derive(Debug)]
struct LogResponse {
    status: StatusCode,
    cached: bool,
    url: String,
    duration: u128,
    varnish_tags: String,
    cookie: String,
}

#[derive(Row, Serialize)]
struct ClickHouseLog {
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
}

#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct Config {
    input_dir: String,
    cookies: String,
    concurrency: i32,
    save_to_clickhouse: bool,
    clickhouse_client: String,
    clickhouse_user: String,
    clickhouse_pwd: String,
    clickhouse_db: String,
    enable_logging: bool,
    simplified_logging: bool,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Usage example: magento_crawler -s /path/to/config/file.json"
)]
struct Args {
    #[arg(
        short = 's',
        long = "settings-file",
        value_name = "SETTINGS_FILE",
        help = "Absolute path to your config.json file. (See how to generate config file at: https://github.com/caduhigueras/magento-crawler)",
        required = true
    )]
    settings_file: String,
}

fn parse_config_file(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let file_exists = exists(path)?;
    if !file_exists {
        let msg = format!("Config file not found on the given location: {}", path);
        return Err(msg.into());
    }

    let config: Config = serde_json::from_reader(File::open(path)?)?;
    Ok(config)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    //---------- Parse config from file
    let args = Args::parse();
    let settings_file = &args.settings_file;
    let config = parse_config_file(&settings_file);

    if config.is_err() {
        eprintln!("Error loading configuration: {:#?}", config.err().unwrap());
        return;
    }

    let config = config.unwrap();

    //---------- Assign config vars
    let input_dir = config.input_dir;
    let cookies_str = config.cookies;
    let concurrency = config.concurrency;
    let clickhouse_client = config.clickhouse_client;
    let clickhouse_user = config.clickhouse_user;
    let clickhouse_pwd = config.clickhouse_pwd;
    let clickhouse_db = config.clickhouse_db;
    let save_to_clickhouse = config.save_to_clickhouse;
    let enable_logging = config.enable_logging;
    let simplified_logging = config.simplified_logging;

    //---------- Clickhouse client
    let ch_client = ChClient::default()
        .with_url(&clickhouse_client)
        .with_user(&clickhouse_user)
        .with_password(&clickhouse_pwd)
        .with_database(&clickhouse_db);

    //---------- How many requests at once
    let concurrency: usize = concurrency as usize;
    let dir_exists = exists(&input_dir).unwrap();

    //---------- Interval between reqs, 5 per second now
    let mut _ticker = interval(Duration::from_millis(200));

    //---------- Build reqwest client
    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    //---------- If input dir doesn't exist, break execution
    if !dir_exists {
        println!("The input directory does not exist. Please check it and try again.");
        return ();
    }

    //---------- Convert cookies in str vector
    let mut cookies = cookies_str
        .split(",")
        .map(String::from)
        .collect::<Vec<String>>();

    //---------- Prepend empty cookie
    if cookies[0] != "" {
        cookies.insert(0, String::from(""));
    }

    //---------- Read files sorted by oldest first. Return if empty
    let files = get_files_from_dir(&input_dir).unwrap();
    if files.is_empty() {
        println!("Directory is empty. Nothing to do.");
        return ();
    }

    //---------- Iterate files
    for file in files {
        let system_now = SystemTime::now();
        let datetime: DateTime<Local> = system_now.into();
        let start_formatted = datetime.format("%Y%m%d%H%M").to_string();

        let start = Instant::now();
        let path = format!("{}/{}", &input_dir, &file);

        //---------- Parse CSV
        let rdr = match ReaderBuilder::new().has_headers(false).from_path(&path) {
            Ok(r) => r,
            Err(_) => {
                println!("Could not read CSV contents from {:?}", path);
                continue;
            }
        };

        //---------- Collect URLs from CSV rows
        let mut urls: Vec<String> = Vec::new();
        for result in rdr.into_deserialize::<CsvRow>() {
            match result {
                Ok(row) => urls.push(row.url),
                Err(e) => {
                    eprintln!("Bad CSV row in {}: {}", path, e);
                    continue;
                }
            }
        }

        if urls.is_empty() {
            println!("No URLs in {}", path);
            continue;
        }

        //---------- Build (url, cookie) jobs
        let jobs = urls.into_iter().flat_map(|url| {
            cookies
                .iter()
                .cloned()
                .map(move |cookie| (url.clone(), cookie))
        });

        let client = client.clone();

        let results = stream::iter(jobs.map(|(url, cookie)| {
            let client = client.clone();
            let ch_client = ch_client.clone();
            let file = file.clone();
            let start_formatted = start_formatted.clone();
            let enable_logging = enable_logging.clone();
            let simplified_logging = simplified_logging.clone();

            async move {
                // Space out task starts (does not block other running tasks)
                // ticker.tick().await;

                match crawl_page(
                    &client,
                    &url,
                    &cookie,
                    save_to_clickhouse,
                    &ch_client,
                    &file,
                    &start_formatted,
                    enable_logging,
                    simplified_logging,
                )
                .await
                {
                    Ok(FollowUpAction::Continue) => Ok::<_, reqwest::Error>(()),
                    Ok(FollowUpAction::Sleep) => {
                        warn!("Waiting 5 min. before resuming (triggered by {})", url);
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
        .buffer_unordered(concurrency) // <= cap in-flight requests
        .collect::<Vec<_>>()
        .await;

        let len = results.len();
        let duration = start.elapsed();
        let secs = duration.as_secs() as f64;
        let minutes = secs / 60.00;

        println!("Job executed. File: {}", path);
        println!("{} in requests {:.2?} minutes", len, minutes);

        let history_dir_path = format!("{}/.history", input_dir);
        let history_output_dir_exists = exists(&history_dir_path).unwrap();

        if !history_output_dir_exists {
            create_dir(&history_dir_path).expect("Failed to create .history dir");
        }
        let history_filename = format!("{}_{}", start_formatted, file);

        // let input_path
        let output_path = format!("{}/.history/{}", &input_dir, &history_filename);
        rename(&path, &output_path).expect("Failed to move file");
    }
}

fn get_files_from_dir(dir: &str) -> Result<Vec<String>, std::io::Error> {
    let mut files: Vec<String> = Vec::new();

    //---------- Read files from dir and exclude non files
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    //---------- Sort by modification date and fallback to SystemTime::UNIX_EPOCH
    entries.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });

    for entry in entries {
        let path = entry.path();
        files.push(path.file_name().unwrap().to_str().unwrap().to_string());
    }

    Ok(files)
}

async fn crawl_page(
    client: &Client,
    url: &str,
    cookie: &str,
    save_to_clickhouse: bool,
    ch_client: &ChClient,
    crawl_id: &str,
    processing_start: &str,
    enable_logging: bool,
    simplified_logging: bool,
) -> Result<FollowUpAction, Error> {
    let start = Instant::now();

    //---------- Actual REQ
    let res = if cookie == "" {
        client.get(url).send().await?
    } else {
        client
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

    if enable_logging {
        if simplified_logging {
            let cached_text = if response.cached {
                String::from("Cached")
            } else {
                String::from("Not Cached")
            };

            info!(
                "{} | {} | {} | {}ms",
                response.url, response.status, cached_text, response.duration
            );
        } else {
            info!(
                "{} - {} | Cached: {} | {}ms | Tags: {} | Cookie: {}",
                response.url,
                response.status,
                response.cached,
                response.duration,
                response.varnish_tags,
                response.cookie
            );
        }
    }

    //---------- Append formatted timestamp so we can always see watch crawl individually in grafana
    let formatted_crawl_id = format!("{}_{}", crawl_id, processing_start);

    //---------- Save into ClickHouse
    if save_to_clickhouse {
        let mut insert: Insert<ClickHouseLog> = ch_client.insert("crawl_logs").unwrap();
        let log_data = ClickHouseLog {
            ts: Utc::now(),
            crawl_id: formatted_crawl_id,
            status: status.as_u16(),
            cached: u8::from(cached),
            url: url.to_string(),
            duration_ms: msecs as u32,
            varnish_tags: varnish_tags.to_string(),
            cookie_hash: cookie.to_string(),
        };

        insert
            .write(&log_data)
            .await
            .expect("Error writing to clickhouse");
        insert.end().await.unwrap();
    }

    //---------- If server is overwhelmed, set sleep
    if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(FollowUpAction::Sleep);
    }

    Ok(FollowUpAction::Continue)
}
