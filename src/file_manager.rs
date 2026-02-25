use csv::ReaderBuilder;
use std::{fs, time::SystemTime};

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
