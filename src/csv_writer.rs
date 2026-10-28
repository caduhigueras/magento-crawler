use std::path::Path;

use futures::io;
use serde::Serialize;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
pub struct CsvRow {
    pub url: String,
    pub status: String,
}

/// Sender type you can clone and pass into your crawl tasks.
pub type CsvSender = mpsc::Sender<CsvRow>;

pub fn spawn_csv_writer<P: AsRef<Path>>(
    path: P,
    channel_capacity: usize,
) -> (CsvSender, tokio::task::JoinHandle<io::Result<()>>) {
    let (tx, mut rx) = mpsc::channel::<CsvRow>(channel_capacity);
    let path = path.as_ref().to_path_buf();

    let handle = tokio::spawn(async move {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let std_file = file.into_std().await;
        let buf = std::io::BufWriter::new(std_file);

        let mut wtr = csv::WriterBuilder::new()
            .has_headers(false)
            .from_writer(buf);

        while let Some(row) = rx.recv().await {
            // csv::Writer::serialize returns csv::Result,
            // which we convert into io::Error manually.
            if let Err(err) = wtr.serialize(row) {
                return Err(io::Error::other(err));
            }
        }

        wtr.flush()?;
        Ok(())
    });

    (tx, handle)
}

/// For the long-running `serve` crawl. Appends every row to
/// `<reports_folder>/YYYY/MM/YYYYmmdd_errors_<suffix>.csv` and flushes it right away,
/// so the file rotates daily and rows aren't lost when the process stops.
pub fn spawn_daily_csv_writer(
    reports_folder: String,
    suffix: &'static str,
    channel_capacity: usize,
) -> (CsvSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<CsvRow>(channel_capacity);

    let handle = tokio::spawn(async move {
        while let Some(row) = rx.recv().await {
            let now = chrono::Local::now();
            let dir = Path::new(&reports_folder)
                .join(now.format("%Y").to_string())
                .join(now.format("%m").to_string());
            let path = dir.join(format!("{}_errors_{}.csv", now.format("%Y%m%d"), suffix));

            //---------- Errors are rare, so opening the file per row is cheap. Log and keep going on failure
            let result = std::fs::create_dir_all(&dir)
                .and_then(|_| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                })
                .and_then(|file| {
                    let mut wtr = csv::WriterBuilder::new()
                        .has_headers(false)
                        .from_writer(file);
                    wtr.serialize(&row).map_err(io::Error::other)?;
                    wtr.flush()
                });

            if let Err(e) = result {
                tracing::error!("Failed to write error row to {:?}: {}", path, e);
            }
        }
    });

    (tx, handle)
}
