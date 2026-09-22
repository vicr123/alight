use crate::addresses::{Lba, Msf};
use crate::driver::{
    BlankMode, CdrDriver, CdrDriverBufferCapacity, CdrDriverError, CdrDriverModePage,
    CdrSessionFormat, CdrStatusResult, GenericProgress, Progress, Writer,
};
use crate::progress_indication::{ProgressIndication, ProgressIndicationPacket};
use crate::scsi::{ScsiDriver, ScsiError, ScsiOpcode, TestUnitReadyResponse, create_scsi_driver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::debug;

#[derive(Clone)]
pub struct MmcDriver {
    scsi: Arc<dyn ScsiDriver>,
}

impl MmcDriver {
    pub fn new(path: &str) -> Result<Self, CdrDriverError> {
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
    fn boxed_clone(&self) -> Box<dyn CdrDriver> {
        let self_clone = Clone::clone(self);
        Box::new(self_clone)
    }

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
            ScsiOpcode::Blank as u8,
            blank_byte,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ])?;

        // Now wait until we're ready
        let (prod, cons) = async_channel::bounded(1);
        thread::spawn({
            let scsi_driver = self.scsi_driver();
            move || {
                if prod
                    .send_blocking(ProgressIndicationPacket::Data(Progress::new(0, 0)))
                    .is_err()
                {
                    // Don't worry about looking at progress information because no one is listening
                    return;
                }

                loop {
                    match mmc_read_disk_info(&scsi_driver) {
                        Ok(CdrStatusResult::Ready) => {
                            let _ = prod.send_blocking(ProgressIndicationPacket::Complete);
                            return;
                        }
                        Ok(CdrStatusResult::Busy(progress)) => {
                            if prod
                                .send_blocking(ProgressIndicationPacket::Data(Progress::new(
                                    progress.min(u16::MAX) as u64,
                                    u16::MAX as u64,
                                )))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(CdrStatusResult::NotReady)
                        | Err(ScsiError::DriveError {
                            sense_data: Some(_),
                            ..
                        }) => {
                            if prod
                                .send_blocking(ProgressIndicationPacket::Data(Progress::new(0, 0)))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = prod.send_blocking(ProgressIndicationPacket::Err(e));
                            return;
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });

        Ok(ProgressIndication::new(cons))
    }

    fn status(&self) -> Result<CdrStatusResult, ScsiError> {
        mmc_ready(&self.scsi_driver())
    }

    fn start_write_session(
        &self,
        dry: bool,
        protect_underrun: bool,
        session_format: CdrSessionFormat,
    ) -> Result<(), CdrDriverError> {
        let Some(mut mode_page) = self.get_mode_page(5)? else {
            return Err(CdrDriverError::InvalidResponse);
        };

        mode_page.result[0] &= 0x7f;
        mode_page.result[2] &= 0xe0;
        mode_page.result[2] |= 0x02;

        if dry {
            mode_page.result[2] |= 1 << 4;
        }

        if protect_underrun {
            mode_page.result[2] |= 0x40;
        } else {
            mode_page.result[2] &= !0x40;
        }

        mode_page.result[3] &= 0x3f;
        mode_page.result[4] &= 0xf0;
        // mode_page.result[4] |= 3;

        mode_page.result[8] = match session_format {
            CdrSessionFormat::CdDigitalAudio => 0x00,
            CdrSessionFormat::CdRom => 0x00,
        };

        self.set_mode_page(&mode_page.result, Some(&mode_page.header), None)?;

        Ok(())
    }

    fn calibrate_laser_power(&self) -> Result<(), ScsiError> {
        debug!("Performing OPC");
        self.scsi_driver().send_cmd(&[
            ScsiOpcode::SendOpcInformation as u8,
            0x1,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
        ])?;
        debug!("OPC complete");
        Ok(())
    }

    fn get_mode_page(&self, page_code: u8) -> Result<Option<CdrDriverModePage>, ScsiError> {
        let data_len = 255_usize;
        let response = self.scsi_driver().send_cmd_with_output(&[
            ScsiOpcode::ModeSense as u8,
            0x0,
            page_code & 0x3f,
            0x0,
            0x0,
            0x0,
            0x0,
            (data_len >> 8) as u8,
            data_len as u8,
        ])?;
        let mode_data_len = (response[0] as usize) << 8 | response[1] as usize;
        let block_desc_len = (response[6] as usize) << 8 | response[7] as usize;

        if mode_data_len > block_desc_len + 6 {
            let mode_page_start = block_desc_len + 8;
            let mode_page_len = response[mode_page_start + 1] as usize + 2;
            Ok(Some(CdrDriverModePage {
                header: (&response[0..8]).try_into().unwrap(),
                block_descriptor: if block_desc_len >= 8 {
                    (&response[8..16]).try_into().unwrap()
                } else {
                    [0; 8]
                },
                result: (&response[mode_page_start..(mode_page_start + mode_page_len)]).into(),
            }))
        } else {
            Ok(None)
        }
    }

    fn set_mode_page(
        &self,
        mode_page: &[u8],
        mode_page_header: Option<&[u8; 8]>,
        block_descriptor: Option<&[u8; 8]>,
    ) -> Result<(), ScsiError> {
        let page_len = mode_page[1] as usize + 2;
        if mode_page.len() < page_len {
            panic!(
                "set_mode_page: invalid mode page length. Data was of length {}, but mode page says {}",
                mode_page.len(),
                page_len
            );
        }

        let data_length = page_len + 8 + if block_descriptor.is_some() { 8 } else { 0 };
        let mut data = vec![0; data_length];

        if let Some(mode_page_header) = mode_page_header {
            data[..8].copy_from_slice(mode_page_header);
        }
        data[0] = 0;
        data[1] = 0;
        data[4] = 0;
        data[5] = 0;

        if let Some(block_descriptor) = block_descriptor {
            data[8..16].copy_from_slice(block_descriptor);
            data[16..].copy_from_slice(mode_page);
            data[6] = 0;
            data[7] = 8;
        } else {
            data[8..].copy_from_slice(mode_page);
            data[6] = 0;
            data[7] = 0;
        }

        self.scsi_driver().send_cmd_with_input(
            &[
                ScsiOpcode::ModeSelect as u8,
                0x1 << 4,
                0x0,
                0x0,
                0x0,
                0x0,
                0x0,
                (data_length >> 8) as u8,
                data_length as u8,
                0x0,
                0x0,
            ],
            &data,
        )?;

        Ok(())
    }

    fn send_cue_sheet(&self, cue_sheet: &[u8]) -> Result<(), CdrDriverError> {
        self.scsi_driver().send_cmd_with_input(
            &[
                ScsiOpcode::SendCueSheet as u8,
                0x0,
                0x0,
                0x0,
                0x0,
                0x0,
                (cue_sheet.len() >> 16) as u8,
                (cue_sheet.len() >> 8) as u8,
                cue_sheet.len() as u8,
                0x0,
            ],
            cue_sheet,
        )?;

        Ok(())
    }

    fn start_write10(&self, address: Lba) -> Result<Writer, CdrDriverError> {
        let driver = self.scsi.clone();
        let block_size = 2352;
        Ok(Writer {
            block_size,
            address,
            queued_writes: Default::default(),
            queued_at: address,
            perform_write: Box::new(move |data, address| {
                let transfer_blocks = data.len() / block_size;
                debug!(
                    "Writing {} bytes to address {} ({} blocks)",
                    data.len(),
                    Msf::from(address),
                    transfer_blocks
                );
                let address = address.0 as i32 - 150;
                let address = address.to_be_bytes();
                driver.send_cmd_with_input(
                    &[
                        ScsiOpcode::Write10 as u8,
                        0x0,
                        address[0],
                        address[1],
                        address[2],
                        address[3],
                        0x0,
                        (transfer_blocks >> 8) as u8,
                        (transfer_blocks & 0xFF) as u8,
                        0x0,
                    ],
                    data,
                )?;
                Ok(())
            }),
        })
    }

    fn flush_cache(&self) -> Result<(), CdrDriverError> {
        self.scsi_driver().send_cmd(&[
            ScsiOpcode::FlushCache as u8,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
        ])?;
        Ok(())
    }

    fn set_speed_multiplier(&self, speed_multiplier: Option<u8>) -> Result<(), CdrDriverError> {
        let speed = speed_multiplier
            .map(|speed| speed as u16 * 117)
            .unwrap_or(0xFFFF);
        self.scsi_driver().send_cmd(&[
            ScsiOpcode::SetCdSpeed as u8,
            0x0,
            (speed >> 8) as u8,
            (speed & 0xFF) as u8,
            (speed >> 8) as u8,
            (speed & 0xFF) as u8,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
        ])?;
        Ok(())
    }

    fn read_buffer_capacity(&self) -> Result<CdrDriverBufferCapacity, CdrDriverError> {
        let response = self.scsi_driver().send_cmd_with_output(&[
            ScsiOpcode::ReadBufferCapacity as u8,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            12,
            0x0,
        ])?;
        Ok(CdrDriverBufferCapacity {
            capacity: u32::from_be_bytes([response[4], response[5], response[6], response[7]]),
            available: u32::from_be_bytes([response[8], response[9], response[10], response[11]]),
        })
    }

    fn next_write_address(&self) -> Result<Lba, CdrDriverError> {
        let info_block_len = 0_i16.to_be_bytes();
        let response = self.scsi_driver().send_cmd_with_output(&[ScsiOpcode::ReadTrackInformation as u8, 0x1, 0x0, 0x0, 0x0, 0xFF, 0x0, info_block_len[0], info_block_len[1], 0x0])?;
        if response[6] & 0x40 > 0 && response[7] & 0x1 > 0 && response[6] & 0xb0 == 0 {
            Ok(Lba(u32::from_be_bytes([response[12], response[13], response[14], response[15]]) as u64 + 150))
        } else {
            Err(CdrDriverError::InvalidResponse)
        }
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
    match scsi_driver.send_cmd_with_output(&[
        ScsiOpcode::ReadDiskInfo as u8,
        0x0,
        0x0,
        0x0,
        0x0,
        0x0,
        0x0,
        0x0,
        0x4,
        0x0,
    ]) {
        Ok(_) => Ok(CdrStatusResult::Ready),
        Err(ScsiError::DriveError {
            sense_data: Some(sense_data),
            ..
        }) => {
            if sense_data.len() >= 14
                && sense_data[2] & 0x0F == 2
                && sense_data[7] >= 6
                && sense_data[12] == 0x4
                && (sense_data[13] == 0x8 || sense_data[13] == 0x7)
            {
                // Not ready, long write in progress
                if sense_data.len() >= 18 && sense_data[15] & 0x80 != 0 {
                    Ok(CdrStatusResult::Busy(u16::from_be_bytes([
                        sense_data[16],
                        sense_data[17],
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
    scsi_driver.send_cmd(&[
        ScsiOpcode::PreventAllowMediumRemoval as u8,
        0x0,
        0x0,
        0x0,
        if lock { 0x1 } else { 0x0 },
        0x0,
    ])
}

fn mmc_load_unload(scsi_driver: &Arc<dyn ScsiDriver>, load: bool) -> Result<(), ScsiError> {
    scsi_driver.send_cmd(&[
        ScsiOpcode::StartStopUnit as u8,
        0x0,
        0x0,
        0x0,
        if load { 0x3 } else { 0x2 },
        0x0,
    ])
}
