use crate::configuration::Settings;
use chrono::{DateTime, Local};
use csv::ReaderBuilder;
use std::io::Read;
use std::{
    fs::{self, File, create_dir_all, exists},
    time::SystemTime,
};

#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    url: String,
}

pub fn get_files_from_dir(dir: &str) -> Result<Vec<String>, std::io::Error> {
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

pub fn parse_csv_as_urls(path: &str) -> Vec<String> {
    //---------- Collect URLs from CSV rows
    let mut urls: Vec<String> = Vec::new();

    //---------- Parse CSV
    let rdr = match ReaderBuilder::new().has_headers(false).from_path(path) {
        Ok(r) => r,
        Err(_) => {
            println!("Could not read CSV contents from {:?}", path);
            return urls;
        }
    };

    for result in rdr.into_deserialize::<CsvRow>() {
        match result {
            Ok(row) => urls.push(row.url),
            Err(e) => {
                eprintln!("Bad CSV row in {}: {}", path, e);
                continue;
            }
        }
    }

    urls
}

pub fn check_and_create_history_folder(config: &Settings, datetime: DateTime<Local>) -> String {
    let year = datetime.format("%Y").to_string();
    let month = datetime.format("%m").to_string();
    let history_dir_path = format!(
        "{}/.history/{}/{}",
        &config.application.input_dir, year, month
    );

    let history_output_dir_exists = exists(&history_dir_path).unwrap();

    if !history_output_dir_exists {
        create_dir_all(&history_dir_path).expect("Failed to create .history dir");
    }

    history_dir_path
}

pub fn check_and_create_csv_errors_dir(config: &Settings, datetime: &DateTime<Local>) -> String {
    let year = datetime.format("%Y").to_string();
    let month = datetime.format("%m").to_string();
    let reports_dir_path = format!("{}/{}/{}", config.application.reports_folder, year, month);

    if !exists(&reports_dir_path).unwrap() {
        create_dir_all(&reports_dir_path).expect("Failed to create reports dir");
    }

    reports_dir_path
}

pub fn has_at_least_one_line(reports_file_path: &str) -> bool {
    let mut file = File::open(reports_file_path).expect("Error opening CSV");
    let mut buffer = [0u8; 1];

    let bytes_read = file.read(&mut buffer).expect("Error reading CSV");

    bytes_read > 0
}
