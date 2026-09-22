use crate::addresses::{Lba, Msf};
use crate::cue_sheet::{
    CueSheet, CueSheetControl, CueSheetDataForm, CueSheetDataFormSubchannel, CueSheetTransition,
    TRACK_LEAD_OUT,
};
use crate::driver::{CdrDriver, CdrDriverError, CdrSessionFormat, CdrStatusResult, Progress};
use crate::progress_indication::{ProgressIndication, ProgressIndicationPacket};
use async_channel::Sender;
use smol::Timer;
use std::collections::HashMap;
use std::iter;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

pub struct BurnDaoAudioCdTrack {
    length: u32,
    producer: Box<dyn FnMut(&mut [u8; 2352]) -> () + Send + Sync>,
}

impl BurnDaoAudioCdTrack {
    pub fn new(
        length: u32,
        producer: impl FnMut(&mut [u8; 2352]) -> () + Send + Sync + 'static,
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

impl BurnDaoAudioCdJob {
    pub fn new() -> BurnDaoAudioCdJob {
        Self {
            dry: false,
            tracks: vec![],
        }
    }

    pub fn set_dry(&mut self, dry: bool) {
        self.dry = dry;
    }

    pub fn push_track(&mut self, track: BurnDaoAudioCdTrack) {
        self.tracks.push(track);
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
        data_form_subchannel: CueSheetDataFormSubchannel::NoSubchannel,
        data_form: CueSheetDataForm::Rom1,
        scms: 0,
        address: Msf::default(),
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
        current_address = current_address + Lba(track.length as u64);
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
            let mut writer = driver.start_write10(Lba::default())?;

            // Write 150 sectors of 0 for lead-in
            for i in 1..=150 {
                writer.write(&[0; 2352])?;
                progress.task_progress.insert(
                    BurnDaoAudioCdJobProgressTask::WriteLeadIn,
                    Progress::new(i, 150),
                );
                let _ = prod.send_blocking(progress.clone().into());
            }

            let mut frame = [0_u8; 2352];
            for (track_index, mut track) in tracks.into_iter().enumerate() {
                let length = track.length;

                debug!(
                    "Start writing track {}, starting at {}, track length {}",
                    track_index,
                    Msf::from(writer.address()),
                    Msf::from(Lba(length as u64))
                );
                progress.current_task =
                    BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8);
                progress.task_progress.insert(
                    BurnDaoAudioCdJobProgressTask::WriteTrack(track_index as u8),
                    Progress::new(0, length as u64),
                );

                // Write all the track data
                for i in 0..length {
                    (track.producer)(&mut frame);
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
