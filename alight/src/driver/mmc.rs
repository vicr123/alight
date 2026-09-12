use crate::driver::{BlankMode, CdrDriver, CdrStatusResult, GenericProgress, Progress};
use crate::progress_indication::{ProgressIndication, ProgressIndicationPacket};
use crate::scsi;
use crate::scsi::{
    ScsiDirection, ScsiDriver, ScsiError, TestUnitReadyResponse, create_scsi_driver,
};
use async_ringbuf::AsyncRb;
use async_ringbuf::producer::AsyncProducer;
use async_ringbuf::traits::Split;
use std::fmt::{Display, Formatter};
use std::fs::OpenOptions;
use std::io::Error;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub enum MmcError {
    IoError(std::io::Error),
    ScsiError(ScsiError),
}

impl From<std::io::Error> for MmcError {
    fn from(value: Error) -> Self {
        Self::IoError(value)
    }
}

impl From<ScsiError> for MmcError {
    fn from(value: ScsiError) -> Self {
        Self::ScsiError(value)
    }
}

impl Display for MmcError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            MmcError::IoError(e) => Display::fmt(e, f),
            MmcError::ScsiError(e) => Display::fmt(e, f),
        }
    }
}

pub struct MmcDriver {
    scsi: Arc<dyn ScsiDriver>,
}

impl MmcDriver {
    pub fn new(path: &str) -> Result<Self, MmcError> {
        Ok(Self::new_from_scsi_driver(Arc::new(create_scsi_driver(
            path,
        )?)))
    }

    pub fn new_from_scsi_driver(scsi_driver: Arc<dyn ScsiDriver>) -> Self {
        Self { scsi: scsi_driver }
    }

    pub fn scsi_driver(&self) -> Arc<dyn ScsiDriver> {
        self.scsi.clone()
    }
}

impl CdrDriver for MmcDriver {
    fn lock_media(&self) -> Result<(), ScsiError> {
        mmc_lock_media(&self.scsi, true)
    }

    fn unlock_media(&self) -> Result<(), ScsiError> {
        mmc_lock_media(&self.scsi, false)
    }

    fn eject(&self) -> Result<(), ScsiError> {
        self.unlock_media()?;
        mmc_load_unload(&self.scsi, false)
    }

    fn close_tray(&self) -> Result<(), ScsiError> {
        mmc_load_unload(&self.scsi, true)
    }

    fn blank(&self, blank_mode: BlankMode) -> Result<GenericProgress, ScsiError> {
        let blank_byte = match blank_mode {
            BlankMode::Fast => 0x11,
            BlankMode::Full => 0x10,
        };

        self.scsi_driver().send_cmd(&[
            0xA1, blank_byte, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ])?;

        // Now wait until we're ready
        let (mut prod, cons) = AsyncRb::new(1).split();
        thread::spawn({
            let scsi_driver = self.scsi_driver();
            move || {
                loop {
                    if smol::block_on(
                        prod.push(ProgressIndicationPacket::Data(Progress::new(0, 0))),
                    )
                    .is_err()
                    {
                        // Don't worry about looking at progress information because no one is listening
                        return;
                    }

                    thread::sleep(Duration::from_secs(1));
                    match mmc_read_disk_info(&scsi_driver) {
                        Ok(CdrStatusResult::Ready) => {
                            let _ = smol::block_on(prod.push(ProgressIndicationPacket::Complete));
                            return;
                        }
                        Ok(CdrStatusResult::Busy(progress)) => {
                            if smol::block_on(prod.push(ProgressIndicationPacket::Data(
                                Progress::new(progress.min(u16::MAX) as u64, u16::MAX as u64),
                            )))
                            .is_err()
                            {
                                return;
                            }
                        }
                        Ok(CdrStatusResult::NotReady)
                        | Err(ScsiError::DriveError {
                            sense_data: Some(_),
                        }) => {
                            if smol::block_on(
                                prod.push(ProgressIndicationPacket::Data(Progress::new(0, 0))),
                            )
                            .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = smol::block_on(prod.push(ProgressIndicationPacket::Err(e)));
                            return;
                        }
                    }
                }
            }
        });

        Ok(ProgressIndication::new(cons))
    }

    fn status(&self) -> Result<CdrStatusResult, ScsiError> {
        mmc_ready(&self.scsi_driver())
    }
}

fn mmc_ready(scsi_driver: &Arc<dyn ScsiDriver>) -> Result<CdrStatusResult, ScsiError> {
    match scsi_driver.test_unit_ready()? {
        TestUnitReadyResponse::Ready => {
            // Check the READ DISK INFO command
            mmc_read_disk_info(scsi_driver)
        }
        TestUnitReadyResponse::Busy => Ok(CdrStatusResult::Busy(0)),
        TestUnitReadyResponse::NoMedia => Ok(CdrStatusResult::NotReady),
    }
}

fn mmc_read_disk_info(scsi_driver: &Arc<dyn ScsiDriver>) -> Result<CdrStatusResult, ScsiError> {
    match scsi_driver.send_cmd_with_output(&[0x51, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x4, 0x0]) {
        Ok(_) => Ok(CdrStatusResult::Ready),
        Err(ScsiError::DriveError {
            sense_data: Some(sense_data),
        }) => {
            if sense_data.len() >= 14
                && sense_data[2] & 0x0F == 2
                && sense_data[7] >= 6
                && sense_data[12] == 0x4
                && (sense_data[13] == 0x8 || sense_data[13] == 0x7)
            {
                // Not ready, long write in progress
                if sense_data.len() >= 18 && sense_data[7] < 10 && sense_data[15] & 0x80 == 0 {
                    Ok(CdrStatusResult::Busy(u16::from_be_bytes([
                        sense_data[14],
                        sense_data[15],
                    ])))
                } else {
                    Ok(CdrStatusResult::NotReady)
                }
            } else {
                Ok(CdrStatusResult::Ready)
            }
        }
        _ => Ok(CdrStatusResult::NotReady),
    }
}

fn mmc_lock_media(scsi_driver: &Arc<dyn ScsiDriver>, lock: bool) -> Result<(), ScsiError> {
    scsi_driver.send_cmd(&[0x1E, 0x0, 0x0, 0x0, if lock { 0x1 } else { 0x0 }, 0x0])
}

fn mmc_load_unload(scsi_driver: &Arc<dyn ScsiDriver>, load: bool) -> Result<(), ScsiError> {
    scsi_driver.send_cmd(&[0x1B, 0x0, 0x0, 0x0, if load { 0x3 } else { 0x2 }, 0x0])
}
