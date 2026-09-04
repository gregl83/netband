use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::config::OutputTarget;
use crate::console::ConsoleSink;
use crate::model::MeasurementEvent;

pub const CSV_HEADER: &str = "schema_version,run_id,event_id,scheduled_at_utc,started_at_utc,finished_at_utc,interface,source_ip,event_kind,trigger_reason,load_phase,load_run_id,target,sequence,outcome,duration_ms,rtt_ms,packets_sent,packets_received,packet_loss_pct,icmp_type,icmp_code,provider_id,provider_kind,server,remote_ip,request_stage,request_attempt,http_status,retry_after_ms,rate_limit_until_utc,daily_runs_used,download_mbps,upload_mbps,bytes_sent,bytes_received,tcp_min_rtt_ms,tcp_rtt_ms,tcp_retransmissions,os_error_code,error_kind,error_message";

const CSV_FIELDS: [&str; 42] = [
    "schema_version",
    "run_id",
    "event_id",
    "scheduled_at_utc",
    "started_at_utc",
    "finished_at_utc",
    "interface",
    "source_ip",
    "event_kind",
    "trigger_reason",
    "load_phase",
    "load_run_id",
    "target",
    "sequence",
    "outcome",
    "duration_ms",
    "rtt_ms",
    "packets_sent",
    "packets_received",
    "packet_loss_pct",
    "icmp_type",
    "icmp_code",
    "provider_id",
    "provider_kind",
    "server",
    "remote_ip",
    "request_stage",
    "request_attempt",
    "http_status",
    "retry_after_ms",
    "rate_limit_until_utc",
    "daily_runs_used",
    "download_mbps",
    "upload_mbps",
    "bytes_sent",
    "bytes_received",
    "tcp_min_rtt_ms",
    "tcp_rtt_ms",
    "tcp_retransmissions",
    "os_error_code",
    "error_kind",
    "error_message",
];

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("journal CSV serialization failed: {0}")]
    Csv(#[from] csv::Error),
    #[error("existing output has an incompatible CSV header: {0}")]
    Header(PathBuf),
    #[error("existing output contains a malformed CSV record: {0}")]
    Corrupt(PathBuf),
    #[error("output file is already locked by another Netband process: {0}")]
    Locked(PathBuf),
}

impl JournalError {
    pub fn write(error: io::Error) -> Self {
        Self::Io(error)
    }

    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Io(source) if source.kind() == io::ErrorKind::PermissionDenied)
    }
}

pub struct Journal<W: Write> {
    writer: csv::Writer<W>,
    sync: Option<fn(&W) -> io::Result<()>>,
}

impl<W: Write> fmt::Debug for Journal<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Journal").finish_non_exhaustive()
    }
}

impl<W: Write> Journal<W> {
    pub fn from_writer(writer: W) -> Result<Self, JournalError> {
        let mut journal = Self::without_header(writer);
        journal.writer.write_record(CSV_FIELDS)?;
        journal.writer.flush()?;
        Ok(journal)
    }

    fn without_header(writer: W) -> Self {
        Self {
            writer: csv::WriterBuilder::new()
                .has_headers(false)
                .terminator(csv::Terminator::CRLF)
                .from_writer(writer),
            sync: None,
        }
    }

    pub fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        for event in events {
            self.writer.serialize(event.sanitized())?;
        }
        self.flush()?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), JournalError> {
        self.writer.flush()?;
        if let Some(sync) = self.sync {
            sync(self.writer.get_ref())?;
        }
        Ok(())
    }

    pub fn into_inner(self) -> Result<W, JournalError> {
        self.writer
            .into_inner()
            .map_err(|error| JournalError::Io(error.into_error()))
    }
}

impl Journal<File> {
    pub fn open_at(
        output: &OutputTarget,
        started_at: DateTime<Utc>,
    ) -> Result<(Self, PathBuf), JournalError> {
        match output {
            OutputTarget::File(path) => Self::open_explicit(path),
            OutputTarget::Directory(directory) => Self::create_timestamped(directory, started_at),
        }
    }

    fn open_explicit(path: &Path) -> Result<(Self, PathBuf), JournalError> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        lock_file(&file, path)?;
        let empty = file.metadata()?.len() == 0;
        if !empty {
            validate_header(&mut file, path)?;
            recover_trailing_record(&mut file, path)?;
        }
        file.seek(SeekFrom::End(0))?;
        let mut journal = if empty {
            Self::from_writer(file)?
        } else {
            Self::without_header(file)
        };
        journal.sync = Some(File::sync_data);
        journal.flush()?;
        Ok((journal, path.to_path_buf()))
    }

    fn create_timestamped(
        directory: &Path,
        started_at: DateTime<Utc>,
    ) -> Result<(Self, PathBuf), JournalError> {
        let filename = format!("netband-{}.csv", started_at.format("%Y%m%dT%H%M%S%.3fZ"));
        let path = directory.join(filename);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut journal = Self::from_writer(file)?;
        journal.sync = Some(File::sync_data);
        journal.flush()?;
        Ok((journal, path))
    }
}

fn lock_file(file: &File, path: &Path) -> Result<(), JournalError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(JournalError::Locked(path.to_path_buf())),
        Err(std::fs::TryLockError::Error(source)) => Err(JournalError::Io(source)),
    }
}

fn validate_header(file: &mut File, path: &Path) -> Result<(), JournalError> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut header)?;
    while matches!(header.last(), Some(b'\n' | b'\r')) {
        header.pop();
    }
    if header != CSV_HEADER.as_bytes() {
        return Err(JournalError::Header(path.to_path_buf()));
    }
    Ok(())
}

fn recover_trailing_record(file: &mut File, path: &Path) -> Result<(), JournalError> {
    let length = file.metadata()?.len();
    if length == CSV_HEADER.len() as u64 {
        file.write_all(b"\r\n")?;
        file.sync_data()?;
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    let terminated = matches!(last[0], b'\n' | b'\r');

    file.seek(SeekFrom::Start(0))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(&mut *file);
    let mut record = csv::ByteRecord::new();
    let mut record_number = 0_u64;
    let mut truncate_at = None;
    while reader.read_byte_record(&mut record)? {
        let start = record
            .position()
            .map_or_else(|| reader.position().byte(), csv::Position::byte);
        let end = reader.position().byte();
        let structurally_valid = record.len() == CSV_FIELDS.len()
            && record
                .iter()
                .all(|field| std::str::from_utf8(field).is_ok());

        if record_number > 0 && !structurally_valid {
            if !terminated && end == length {
                truncate_at = Some(start);
                break;
            }
            return Err(JournalError::Corrupt(path.to_path_buf()));
        }
        if record_number > 0 && !terminated && end == length {
            truncate_at = Some(start);
            break;
        }
        record_number += 1;
    }
    drop(reader);

    if let Some(mut offset) = truncate_at {
        file.seek(SeekFrom::Start(offset))?;
        let mut boundary = [0];
        if file.read(&mut boundary)? == 1 && boundary[0] == b'\n' {
            offset += 1;
        }
        file.set_len(offset)?;
        file.sync_data()?;
        tracing::warn!(
            path = %path.display(),
            truncated_bytes = length - offset,
            "discarded incomplete trailing CSV record"
        );
    }
    Ok(())
}

pub trait JournalSink {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError>;

    fn flush(&mut self) -> Result<(), JournalError> {
        Ok(())
    }
}

impl<W: Write> JournalSink for Journal<W> {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        Journal::append_batch(self, events)
    }

    fn flush(&mut self) -> Result<(), JournalError> {
        Journal::flush(self)
    }
}

pub struct OutputCoordinator<J, C> {
    journal: J,
    console: C,
}

impl<J, C> OutputCoordinator<J, C>
where
    J: JournalSink,
    C: ConsoleSink,
{
    pub fn new(journal: J, console: C) -> Self {
        Self { journal, console }
    }

    pub fn publish_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        self.journal.append_batch(events)?;
        crate::diagnostics::record_events(events);
        for event in events {
            self.console.offer(event);
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), JournalError> {
        self.journal.flush()
    }

    pub fn into_parts(self) -> (J, C) {
        (self.journal, self.console)
    }
}
