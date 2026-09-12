use crate::progress_indication::ProgressIndication;
use crate::scsi::ScsiError;

pub mod mmc;

#[derive(Debug, Copy, Clone, Default)]
pub enum BlankMode {
    #[default]
    Fast,
    Full
}

pub struct Progress {
    pub progress: u64,
    pub total: u64
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
    Busy(u16)
}

pub trait CdrDriver {
    fn lock_media(&self) -> Result<(), ScsiError>;
    fn unlock_media(&self) -> Result<(), ScsiError>;
    fn eject(&self) -> Result<(), ScsiError>;
    fn close_tray(&self) -> Result<(), ScsiError>;
    fn blank(&self, blank_mode: BlankMode) -> Result<GenericProgress, ScsiError>;
    fn status(&self) -> Result<CdrStatusResult, ScsiError>;
}