use std::fs;
use std::time::{Instant, SystemTime};
use tokio::time::{interval, Duration, sleep};
use reqwest::{Client, Error, StatusCode};
use log::{info, error, warn};
use dotenv::dotenv;
use std::fs::exists;
use csv::ReaderBuilder;
use futures::stream;
use futures::StreamExt;

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

impl LogResponse {
    pub fn new(status: StatusCode, url: String, duration: u128, cached: bool, varnish_tags: String, cookie: String) -> Self {
        LogResponse{
            status,
            url,
            duration,
            cached,
            varnish_tags,
            cookie
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    url: String,
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    //---------- Extract env inputs
    let input_dir = std::env::var("INPUT_DIR").expect("INPUT_DIR must be set in .env");
    let cookies_from_env = std::env::var("COOKIES").expect("COOKIES must be set in .env");
    let concurrency_env = std::env::var("CONCURRENCY").expect("CONCURRENCY must be set in .env");

    //---------- How many requests at once
    let concurrency: usize = concurrency_env
        .parse()
        .expect("CONCURRENCY must be a valid number");

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
    let mut cookies = cookies_from_env.split(",").map(String::from).collect::<Vec<String>>();

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
        let start = Instant::now();
        let path = format!("{}/{}", &input_dir, &file);

        //---------- Parse CSV
        let rdr = match ReaderBuilder::new()
            .has_headers(false)
            .from_path(&path) {
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
            cookies.iter().cloned().map(move |cookie| (url.clone(), cookie))
        });

        let client = client.clone();

        let results = stream::iter(jobs.map(|(url, cookie)| {
            let client = client.clone();

            async move {
                // Space out task starts (does not block other running tasks)
                // ticker.tick().await;

                match crawl_page(&client, &url, &cookie).await {
                    Ok(FollowUpAction::Continue) => Ok::<_, reqwest::Error>(()),
                    Ok(FollowUpAction::Sleep) => {
                        println!("Waiting 5 min. before resuming (triggered by {})", url);
                        sleep(Duration::from_secs(300)).await;
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("Error crawling page {}: {}", url, e);
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

async fn crawl_page(client: &Client, url: &str, cookie: &str) -> Result<FollowUpAction, Error> {
    let start = Instant::now();
    let res = client.get(url).header(
        reqwest::header::COOKIE,
        format!("X-Magento-Vary={}", cookie)
    )        .send().await?;

    let duration = start.elapsed();
    let msecs = duration.as_millis();
    let headers = res.headers();
    let status = res.status();

    //---------- If server is overwhelmed, set sleep
    if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
        let response = LogResponse::new(status, url.to_string(), msecs, false, "".to_string(), cookie.to_string());
        println!("response {:#?}", response);
        error!("{},{},{},{},{},{}", response.url, response.status, response.duration, response.cached, response.varnish_tags, response.cookie);
        return Ok(FollowUpAction::Sleep);
    }

    //---------- If page has errors
    if status == StatusCode::BAD_REQUEST {
        //todo log responses
        let response = LogResponse::new(status, url.to_string(), msecs, false, "".to_string(), cookie.to_string());
        println!("response {:#?}", response);
        error!("{},{},{},{},{},{}", response.url, response.status, response.duration, response.cached, response.varnish_tags, response.cookie);
        return Ok(FollowUpAction::Continue);
    }

    //---------- Track 404s
    if status == StatusCode::NOT_FOUND {
        let response = LogResponse::new(status, url.to_string(), msecs, false, "".to_string(), cookie.to_string());
        println!("response {:#?}", response);
        warn!("{},{},{},{},{},{}", response.url, response.status, response.duration, response.cached, response.varnish_tags, response.cookie);
        return Ok(FollowUpAction::Continue);
    }

    //todo add flag?
    let varnish_tags = if let Some(v) = headers.get("x-varnish") {
        v.to_str().unwrap_or("")
    } else {
        ""
    };

    let tags_vec = varnish_tags.split(" ").collect::<Vec<&str>>();
    let cached = tags_vec.len() > 1;

    //todo log responses
    let response = LogResponse::new(status, url.to_string(), msecs, cached, varnish_tags.to_string(), cookie.to_string());
    println!("response {:#?}", response);
    info!("{},{},{},{},{},{}", response.url, response.status, response.duration, response.cached, response.varnish_tags, response.cookie);

    Ok(FollowUpAction::Continue)
}
