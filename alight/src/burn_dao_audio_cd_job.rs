use crate::addresses::{Lba, Msf};
use crate::cd_text::CdText;
use crate::cue_sheet::{
    CueSheet, CueSheetControl, CueSheetDataForm, CueSheetDataFormSubchannel, CueSheetTransition,
    TRACK_LEAD_OUT,
};
use crate::driver::{CdrDriver, CdrDriverDiscInformation, CdrDriverError, CdrSessionFormat, CdrStatusResult, DiscStatus, Progress};
use crate::progress_indication::{ProgressIndication, ProgressIndicationPacket};
use crate::scsi::{ScsiError, SenseKey};
use async_channel::Sender;
use smol::Timer;
use std::collections::HashMap;
use std::iter;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error};

pub struct BurnDaoAudioCdTrack {
    length: u32,
    producer: Box<dyn FnMut(&mut [u8; 2352]) -> Result<(), String> + Send>,
}

impl BurnDaoAudioCdTrack {
    pub fn new(
        length: u32,
        producer: impl FnMut(&mut [u8; 2352]) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            length,
            producer: Box::new(producer),
        }
    }
}

pub struct BurnDaoAudioCdJob {
    dry: bool,
    tracks: Vec<BurnDaoAudioCdTrack>,
    cd_text: Option<CdText>,
}

#[derive(Clone, Debug)]
pub struct BurnDaoAudioCdJobProgress {
    pub task_progress: HashMap<BurnDaoAudioCdJobProgressTask, Progress>,
    pub task_order: Vec<BurnDaoAudioCdJobProgressTask>,
    pub total_progress: Progress,
    pub current_task: BurnDaoAudioCdJobProgressTask,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum BurnDaoAudioCdJobProgressTask {
    PowerCalibration,
    WriteLeadIn,
    WriteTrack(u8),
    WriteLeadOut,
}

pub enum BurnPossibility {
    Ok,
    EraseRequired,
    NotEnoughSpace,
    NoMedia,
    MediaWritten
}

impl BurnDaoAudioCdJob {
    pub fn new() -> BurnDaoAudioCdJob {
        Self {
            dry: false,
            tracks: vec![],
            cd_text: None,
        }
    }

    pub fn set_dry(&mut self, dry: bool) {
        self.dry = dry;
    }

    pub fn push_track(&mut self, track: BurnDaoAudioCdTrack) {
        self.tracks.push(track);
    }

    pub fn set_cd_text(&mut self, cd_text: CdText) {
        self.cd_text = Some(cd_text);
    }

    pub fn burn(
        self,
        driver: &dyn CdrDriver,
    ) -> Result<ProgressIndication<BurnDaoAudioCdJobProgress, CdrDriverError>, CdrDriverError> {
        let driver = driver.boxed_clone();
        driver.lock_media()?;

        let (mut prod, cons) = async_channel::unbounded();
        smol::spawn({
            async move {
                if let Err(e) = burn(driver, &mut prod, self).await {
                    let _ = prod.send(ProgressIndicationPacket::Err(e)).await;
                }
            }
        })
        .detach();

        Ok(ProgressIndication::new(cons))
    }

    pub fn can_burn(&self, driver: &dyn CdrDriver) -> Result<BurnPossibility, CdrDriverError> {
        let disc_information = match driver.disc_information() {
            Ok(disc_information) => disc_information,
            Err(CdrDriverError::ScsiError(ScsiError::DriveError {
                cmd,
                sense_data: Some(sense_data),
            })) => {
                if sense_data.sense_key() == SenseKey::NotReady
                    && sense_data.additional_sense_code() == Some(0x3A)
                {
                    return Ok(BurnPossibility::NoMedia);
                }

                return Err(CdrDriverError::ScsiError(ScsiError::DriveError {
                    cmd,
                    sense_data: Some(sense_data),
                }));
            }
            Err(e) => {
                return Err(e);
            }
        };

        // TODO: Double check space constraints

        if disc_information.disc_status != DiscStatus::Empty {
            if disc_information.erasable {
                return Ok(BurnPossibility::EraseRequired);
            }
            return Ok(BurnPossibility::MediaWritten);
        }

        Ok(BurnPossibility::Ok)
    }
}

async fn burn(
    driver: Box<dyn CdrDriver>,
    prod: &mut Sender<ProgressIndicationPacket<BurnDaoAudioCdJobProgress, CdrDriverError>>,
    job: BurnDaoAudioCdJob,
) -> Result<(), CdrDriverError> {
    let mut progress = BurnDaoAudioCdJobProgress {
        task_order: iter::once(BurnDaoAudioCdJobProgressTask::PowerCalibration)
            .chain(iter::once(BurnDaoAudioCdJobProgressTask::WriteLeadIn))
            .chain(
                job.tracks
                    .iter()
                    .enumerate()
                    .map(|(i, _)| BurnDaoAudioCdJobProgressTask::WriteTrack(i as u8)),
            )
            .chain(iter::once(BurnDaoAudioCdJobProgressTask::WriteLeadOut))
            .collect(),
        task_progress: HashMap::new(),
        total_progress: Progress::new(0, 0),
        current_task: BurnDaoAudioCdJobProgressTask::PowerCalibration,
    };

    let _ = prod.send(progress.clone().into()).await;

    driver.lock_media()?;
    let disc_information = driver.disc_information()?;
    driver.set_speed_multiplier(None)?;
    driver.start_write_session(job.dry, false, CdrSessionFormat::CdDigitalAudio)?;
    if !job.dry {
        driver.calibrate_laser_power()?;
    }
    wait_for_ready(&driver).await;
    if let Ok(next_write_address) = driver.next_write_address() {
        debug!(
            "Drive reports next write address: {}",
            Msf::from(next_write_address)
        );
    }
    wait_for_ready(&driver).await;

    let mut cue_sheet = CueSheet::new();
    // Add the pregap to the cue sheet
    cue_sheet.push_transition(CueSheetTransition {
        control: CueSheetControl::Audio,
        track: 0,
        index: 0,
        data_form_subchannel: if job.cd_text.is_some() {
            CueSheetDataFormSubchannel::SupplyPackedRW
        } else {
            CueSheetDataFormSubchannel::NoSubchannel
        },
        data_form: CueSheetDataForm::LeadIn,
        scms: 0,
        address: if job.cd_text.is_some() {
            disc_information.lead_in_start.into()
        } else {
            Msf::default()
        },
    });
    cue_sheet.push_transition(CueSheetTransition {
        control: CueSheetControl::Audio,
        track: 1,
        index: 0,
        data_form_subchannel: CueSheetDataFormSubchannel::NoSubchannel,
        data_form: CueSheetDataForm::DigitalAudio,
        scms: 0,
        address: Msf::default(),
    });

    let mut current_address = Msf::seconds(2);
    for (i, track) in job.tracks.iter().enumerate() {
        cue_sheet.push_transition(CueSheetTransition {
            control: CueSheetControl::Audio,
            track: (i + 1) as u8,
            index: 1,
            data_form_subchannel: CueSheetDataFormSubchannel::NoSubchannel,
            data_form: CueSheetDataForm::DigitalAudio,
            scms: 0,
            address: current_address,
        });
        current_address = current_address + Lba(track.length as i32);
    }
    // Add the lead out
    cue_sheet.push_transition(CueSheetTransition {
        control: CueSheetControl::Audio,
        track: TRACK_LEAD_OUT,
        index: 1,
        data_form_subchannel: CueSheetDataFormSubchannel::NoSubchannel,
        data_form: CueSheetDataForm::Rom1,
        scms: 0,
        address: current_address,
    });

    debug!("Cue sheet: {:?}", cue_sheet);

    let cue_sheet = cue_sheet.generate();
    driver.send_cue_sheet(&cue_sheet)?;
    wait_for_ready(&driver).await;

    progress.task_progress.insert(
        BurnDaoAudioCdJobProgressTask::PowerCalibration,
        Progress::new(1, 1),
    );
    progress.current_task = BurnDaoAudioCdJobProgressTask::WriteLeadIn;
    progress.task_progress.insert(
        BurnDaoAudioCdJobProgressTask::WriteLeadIn,
        Progress::new(0, 150),
    );
    for (track_index, track) in job.tracks.iter().enumerate() {
        progress.task_progress.insert(
            BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8),
            Progress::new(0, track.length as u64),
        );
    }
    let _ = prod.send(progress.clone().into()).await;

    let mut progress = smol::unblock({
        let driver = driver.boxed_clone();
        let prod = prod.clone();
        let tracks = job.tracks;
        move || -> Result<BurnDaoAudioCdJobProgress, CdrDriverError> {
            // Write the CD text
            let mut lead_in_progress_total = 150;
            let mut lead_in_progress_base = 0;
            if let Some(cd_text) = job.cd_text {
                lead_in_progress_total += disc_information.lead_in_length.0 as u64;
                lead_in_progress_base = disc_information.lead_in_length.0 as u64;

                // 96 bytes for the CD text
                let mut writer = driver.start_write10(96, -disc_information.lead_in_length)?;

                let mut frame = [0_u8; 96];
                let cd_text = cd_text.into_bytes();

                // Write the CD text
                for write_lba in 0..(disc_information.lead_in_length.0 - 150) {
                    for index in 0..24 {
                        let frame_idx = index * 4;
                        let cd_text_idx = (index * 3 + write_lba as usize * 24 * 3) % cd_text.len();

                        unfold_for_subchannel(
                            &cd_text[cd_text_idx..cd_text_idx + 3].try_into().unwrap(),
                            (&mut frame[frame_idx..frame_idx + 4]).try_into().unwrap(),
                        );
                    }
                    writer.write(&frame)?;
                    progress.task_progress.insert(
                        BurnDaoAudioCdJobProgressTask::WriteLeadIn,
                        Progress::new(write_lba as u64, lead_in_progress_total),
                    );
                    let _ = prod.send_blocking(progress.clone().into());
                }

                writer.flush()?;
            }

            // 2448 bytes for the write mode
            let mut writer = driver.start_write10(2352, Lba::PREGAP_START)?;

            // Write 150 sectors of 0 for pregap
            for i in 1..=150 {
                writer.write(&[0; 2352])?;
                progress.task_progress.insert(
                    BurnDaoAudioCdJobProgressTask::WriteLeadIn,
                    Progress::new(lead_in_progress_base + i as u64, lead_in_progress_total),
                );
                let _ = prod.send_blocking(progress.clone().into());
            }

            let mut frame = [0; 2352];
            for (track_index, mut track) in tracks.into_iter().enumerate() {
                let length = track.length;

                debug!(
                    "Start writing track {}, starting at {}, track length {}",
                    track_index,
                    Msf::from(writer.address()),
                    Msf::from(Lba(length as i32))
                );
                progress.current_task =
                    BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8);
                progress.task_progress.insert(
                    BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8),
                    Progress::new(0, length as u64),
                );

                // Write all the track data
                for i in 0..length {
                    if let Err(e) = (track.producer)(&mut frame) {
                        error!("Error writing track {}: {}", track_index, e);
                        return Err(CdrDriverError::InvalidResponse);
                    }
                    writer.write(&frame)?;
                    progress.task_progress.insert(
                        BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8),
                        Progress::new(i as u64 + 1, length as u64),
                    );
                    let _ = prod.send_blocking(progress.clone().into());
                }
            }
            debug!(
                "All tracks written. Current address {}",
                Msf::from(writer.address())
            );

            writer.flush()?;
            Ok(progress)
        }
    })
    .await?;

    progress.current_task = BurnDaoAudioCdJobProgressTask::WriteLeadOut;
    progress.task_progress.insert(
        BurnDaoAudioCdJobProgressTask::WriteLeadOut,
        Progress::new(0, 0),
    );
    let _ = prod.send(progress.clone().into()).await;

    driver.flush_cache()?;
    wait_for_ready(&driver).await;
    driver.flush_cache()?;

    Ok(())
}

async fn wait_for_ready(driver: &Box<dyn CdrDriver>) {
    loop {
        if let Ok(CdrStatusResult::Ready) = driver.status() {
            break;
        }

        Timer::after(Duration::from_millis(100)).await;
    }
}

fn unfold_for_subchannel(input: &[u8; 3], output: &mut [u8; 4]) {
    output[0] = (input[0] >> 2) & 0b00111111;
    output[1] = ((input[0] << 4) & 0b00110000) | ((input[1] >> 4) & 0b00001111);
    output[2] = ((input[1] << 2) & 0b00111100) | ((input[2] >> 6) & 0b00000011);
    output[3] = input[2] & 0b00111111;
}
