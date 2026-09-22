use crate::addresses::{Lba, Msf};
use crate::progress_indication::ProgressIndication;
use crate::scsi::{ScsiError, ScsiOpcode};
use std::fmt::{Display, Formatter};
use std::io::Error;
use tracing::warn;

pub mod mmc;

#[derive(Debug)]
pub enum CdrDriverError {
    IoError(std::io::Error),
    ScsiError(ScsiError),

    InvalidResponse,
}

impl From<std::io::Error> for CdrDriverError {
    fn from(value: Error) -> Self {
        Self::IoError(value)
    }
}

impl From<ScsiError> for CdrDriverError {
    fn from(value: ScsiError) -> Self {
        Self::ScsiError(value)
    }
}

impl Display for CdrDriverError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CdrDriverError::IoError(e) => Display::fmt(e, f),
            CdrDriverError::ScsiError(e) => Display::fmt(e, f),
            CdrDriverError::InvalidResponse => write!(f, "Invalid response"),
        }
    }
}

#[derive(Debug, Copy, Clone, Default)]
pub enum BlankMode {
    #[default]
    Fast,
    Full,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub progress: u64,
    pub total: u64,
}

impl Progress {
    pub fn new(progress: u64, total: u64) -> Self {
        Self { progress, total }
    }
}

pub type GenericProgress = ProgressIndication<Progress, ScsiError>;

#[derive(Debug, Eq, PartialEq)]
pub enum CdrStatusResult {
    Ready,
    NotReady,
    Busy(u16),
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum CdrSessionFormat {
    CdDigitalAudio,
    CdRom,
}

pub struct CdrDriverModePage {
    result: Vec<u8>,
    header: [u8; 8],
    block_descriptor: [u8; 8],
}

pub struct CdrDriverBufferCapacity {
    pub capacity: u32,
    pub available: u32,
}

pub trait CdrDriver: Send + Sync {
    fn boxed_clone(&self) -> Box<dyn CdrDriver>;
    fn lock_media(&self) -> Result<(), ScsiError>;
    fn unlock_media(&self) -> Result<(), ScsiError>;
    fn eject(&self) -> Result<(), ScsiError>;
    fn close_tray(&self) -> Result<(), ScsiError>;
    fn blank(&self, blank_mode: BlankMode) -> Result<GenericProgress, ScsiError>;
    fn status(&self) -> Result<CdrStatusResult, ScsiError>;

    fn start_write_session(
        &self,
        dry: bool,
        protect_underrun: bool,
        session_format: CdrSessionFormat,
    ) -> Result<(), CdrDriverError>;
    fn calibrate_laser_power(&self) -> Result<(), ScsiError>;

    fn get_mode_page(&self, page_code: u8) -> Result<Option<CdrDriverModePage>, ScsiError>;
    fn set_mode_page(
        &self,
        mode_page: &[u8],
        mode_page_header: Option<&[u8; 8]>,
        block_descriptor: Option<&[u8; 8]>,
    ) -> Result<(), ScsiError>;
    fn send_cue_sheet(&self, cue_sheet: &[u8]) -> Result<(), CdrDriverError>;
    fn start_write10(&self, address: Lba) -> Result<Writer, CdrDriverError>;
    fn flush_cache(&self) -> Result<(), CdrDriverError>;
    fn set_speed_multiplier(&self, speed_multiplier: Option<u8>) -> Result<(), CdrDriverError>;
    fn read_buffer_capacity(&self) -> Result<CdrDriverBufferCapacity, CdrDriverError>;
    fn next_write_address(&self) -> Result<Lba, CdrDriverError>;
}

pub struct Writer {
    block_size: usize,
    address: Lba,
    perform_write: Box<dyn Fn(&[u8], Lba) -> Result<(), CdrDriverError>>,
    queued_writes: Vec<u8>,
    queued_at: Lba,
}

impl Writer {
    pub fn address(&self) -> Lba {
        self.address
    }
}

impl Writer {
    pub fn write(&mut self, data: &[u8]) -> Result<(), CdrDriverError> {
        if data.len() != self.block_size {
            panic!(
                "Received data of length {} but expected {}",
                data.len(),
                self.block_size
            );
        }

        if self.queued_writes.is_empty() {
            self.queued_at = self.address;
        }
        self.queued_writes.extend_from_slice(data);
        self.address.0 += 1;
        if self.queued_writes.len() >= (65536 / self.block_size) * self.block_size {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), CdrDriverError> {
        (self.perform_write)(&self.queued_writes, self.queued_at)?;
        self.queued_writes.clear();
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.queued_writes.is_empty() {
            warn!(
                "Writer dropped at {} with remaining data not written",
                Msf::from(self.queued_at)
            )
        }
    }
}
