use crate::configuration::Settings;
use chrono::{DateTime, Local};
use csv::ReaderBuilder;
use reqwest::Url;
use std::io::Read;
use std::{
    fs::{self, File, create_dir_all, exists},
    path::Path,
    time::SystemTime,
};
use tracing::warn;

#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    url: String,
}

/// Extensions accepted as URL lists. Anything else in the input dir is left untouched.
const ALLOWED_EXTENSIONS: [&str; 2] = ["csv", "txt"];

pub fn get_files_from_dir(dir: &str) -> Result<Vec<String>, std::io::Error> {
    let mut files: Vec<String> = Vec::new();

    //---------- Read files from dir. file_type() doesn't follow symlinks, so links are excluded too
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| match e.file_type() {
            Ok(t) if t.is_file() => true,
            Ok(t) if t.is_symlink() => {
                warn!("Skipping symlink: {:?}", e.path());
                false
            }
            _ => false,
        })
        .collect();

    //---------- Sort by modification date and fallback to SystemTime::UNIX_EPOCH
    entries.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });

    for entry in entries {
        //---------- Skip names that aren't valid UTF-8 instead of aborting the whole run
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            warn!("Skipping file with non UTF-8 name: {:?}", entry.path());
            continue;
        };

        //---------- Skip hidden files (.DS_Store, editor swap files...) silently
        if name.starts_with('.') {
            continue;
        }

        let has_allowed_extension = Path::new(&name)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ALLOWED_EXTENSIONS.contains(&ext.to_lowercase().as_str()));

        if !has_allowed_extension {
            warn!("Skipping {}: only .csv and .txt files are crawled", name);
            continue;
        }

        files.push(name);
    }

    Ok(files)
}

/// Trims and lowercases the configured hosts, dropping empty entries.
pub fn normalize_hosts(hosts: &[String]) -> Vec<String> {
    hosts
        .iter()
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// Only http(s) URLs whose host is exactly one of `allowed_hosts` can be crawled.
fn validate_url(url: &str, allowed_hosts: &[String]) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("scheme '{}' is not allowed", parsed.scheme()));
    }

    match parsed.host_str() {
        Some(host) if allowed_hosts.iter().any(|h| h == host) => Ok(()),
        Some(host) => Err(format!("host '{}' is not in allowed_hosts", host)),
        None => Err(String::from("missing host")),
    }
}

pub fn parse_csv_as_urls(path: &str, allowed_hosts: &[String]) -> Vec<String> {
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
            Ok(row) => {
                let url = row.url.trim();
                match validate_url(url, allowed_hosts) {
                    Ok(()) => urls.push(url.to_string()),
                    Err(reason) => warn!("Skipping URL {} in {}: {}", url, path, reason),
                }
            }
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
        config.application.input_dir, year, month
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        normalize_hosts(&[String::from(" Example.com "), String::new()])
    }

    #[test]
    fn normalize_hosts_trims_lowercases_and_drops_empty() {
        assert_eq!(hosts(), vec![String::from("example.com")]);
    }

    #[test]
    fn validate_url_accepts_allowed_hosts_only() {
        assert!(validate_url("https://example.com/phones", &hosts()).is_ok());
        assert!(validate_url("http://EXAMPLE.com:8080/", &hosts()).is_ok());
        assert!(validate_url("https://evil.com/", &hosts()).is_err());
        assert!(validate_url("https://shop.example.com/", &hosts()).is_err());
        assert!(validate_url("https://example.com.evil.com/", &hosts()).is_err());
        assert!(validate_url("ftp://example.com/", &hosts()).is_err());
        assert!(validate_url("file:///etc/passwd", &hosts()).is_err());
        assert!(validate_url("not a url", &hosts()).is_err());
    }

    #[test]
    fn get_files_from_dir_filters_extensions_hidden_files_and_symlinks() {
        let dir = std::env::temp_dir().join(format!("magento_crawler_test_{}", std::process::id()));
        fs::create_dir_all(dir.join("subdir")).unwrap();
        for name in ["urls.csv", "more.TXT", "notes.md", ".hidden.csv", "noext"] {
            fs::write(dir.join(name), "https://example.com/\n").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("urls.csv"), dir.join("link.csv")).unwrap();

        let mut files = get_files_from_dir(dir.to_str().unwrap()).unwrap();
        files.sort();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            files,
            vec![String::from("more.TXT"), String::from("urls.csv")]
        );
    }

    #[test]
    fn parse_csv_as_urls_skips_disallowed_rows() {
        let path =
            std::env::temp_dir().join(format!("magento_crawler_urls_{}.csv", std::process::id()));
        fs::write(
            &path,
            "https://example.com/a\n https://example.com/b \nhttps://evil.com/c\ngarbage\n",
        )
        .unwrap();

        let urls = parse_csv_as_urls(path.to_str().unwrap(), &hosts());
        fs::remove_file(&path).unwrap();

        assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
    }
}
