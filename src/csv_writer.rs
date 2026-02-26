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
                return Err(io::Error::new(io::ErrorKind::Other, err));
            }
        }

        wtr.flush()?;
        Ok(())
    });

    (tx, handle)
}
