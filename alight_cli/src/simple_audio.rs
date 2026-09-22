use crate::{SimpleAudioArgs, pause_before_operation, progress_style};
use alight::burn_dao_audio_cd_job::{
    BurnDaoAudioCdJob, BurnDaoAudioCdJobProgress, BurnDaoAudioCdJobProgressTask,
    BurnDaoAudioCdTrack,
};
use alight::driver::mmc::MmcDriver;
use cntp_i18n::{tr, tr_error, tr_info, trn_info};
use indicatif::{MultiProgress, ProgressBar};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tracing::{Level, error};

pub fn simple_audio(args: SimpleAudioArgs, mmc: MmcDriver) -> ExitCode {
    let files_len = args.files.len();

    let mut job = BurnDaoAudioCdJob::new();
    job.set_dry(args.dry);
    for file in args.files {
        let mut file = match File::open(&file) {
            Ok(file) => file,
            Err(e) => {
                tr_error!(
                    "FILE_ERROR",
                    "Cannot open file {{file}}: {{error}}",
                    file = file,
                    error = e.to_string()
                );
                return ExitCode::FAILURE;
            }
        };
        let meta = match file.metadata() {
            Ok(meta) => meta,
            Err(e) => {
                tr_error!(
                    "READ_ERROR",
                    "Cannot read file: {{error}}",
                    error = e.to_string()
                );
                return ExitCode::FAILURE;
            }
        };

        if let Err(e) = file.seek(SeekFrom::Start(44)) {
            tr_error!(
                "SEEK_ERROR",
                "Cannot seek file: {{error}}",
                error = e.to_string()
            );
            return ExitCode::FAILURE;
        }

        job.push_track(BurnDaoAudioCdTrack::new(
            ((meta.len() - 44) / 2352) as u32,
            move |frame| {
                file.read(frame).unwrap();
            },
        ))
    }

    trn_info!(
        "BURN_SUMMARY",
        "Burning {{count}} track to disc in {{device}}.",
        "Burning {{count}} tracks to disc in {{device}}.",
        count = files_len,
        device = mmc.scsi_driver().device()
    );

    pause_before_operation();

    let mut progress = match job.burn(&mmc) {
        Ok(progress) => progress,
        Err(e) => {
            tr_error!("BURN_FAIL", "Burn failed: {{error}}", error = e.to_string());
            return ExitCode::FAILURE;
        }
    };

    smol::block_on(async move {
        let started = Instant::now();

        let m = MultiProgress::new();
        let mut bars: HashMap<BurnDaoAudioCdJobProgressTask, ProgressBar> = HashMap::new();

        loop {
            let progress = match progress.next().await {
                None => break,
                Some(Ok(progress)) => progress,
                Some(Err(e)) => {
                    for (_, bar) in &bars {
                        bar.finish();
                    }
                    tr_error!("BURN_FAIL", "Burn failed: {{error}}", error = e.to_string());
                    return ExitCode::FAILURE;
                }
            };

            if !tracing::event_enabled!(Level::DEBUG) {
                if bars.is_empty() {
                    for task in &progress.task_order {
                        let bar = m.add(ProgressBar::new_spinner());
                        bar.enable_steady_tick(Duration::from_millis(100));
                        bar.set_message(task_message(task));
                        bars.insert(*task, bar);
                    }
                }

                for task in &progress.task_order {
                    let bar = &bars[&task];
                    bar.set_elapsed(started.elapsed());
                    if let Some(task_progress) = progress.task_progress.get(task) {
                        bar.set_length(task_progress.total);
                        bar.set_position(task_progress.progress);
                        bar.set_style(progress_style(
                            task_message(task),
                            task == &progress.current_task,
                            task_progress.total == 0,
                        ));
                    } else {
                        bar.set_style(progress_style(
                            task_message(task),
                            task == &progress.current_task,
                            true,
                        ));
                    }
                    bar.tick();
                }
            }
        }

        for (_, bar) in &bars {
            bar.finish();
        }

        tr_info!("BURN_SUCCESS", "Burned.");
        ExitCode::SUCCESS
    })
}

fn task_message(task: &BurnDaoAudioCdJobProgressTask) -> String {
    match task {
        BurnDaoAudioCdJobProgressTask::PowerCalibration => {
            tr!("BURN_POWER_CALIBRATION", "Preparing burn session")
        }
        BurnDaoAudioCdJobProgressTask::WriteLeadIn => {
            tr!("BURN_WRITE_LEAD_IN", "Writing lead-in")
        }
        BurnDaoAudioCdJobProgressTask::WriteTrack(track) => {
            tr!(
                "BURN_WRITE_TRACK",
                "Writing track {{track}}",
                track = (track + 1)
            )
        }
        BurnDaoAudioCdJobProgressTask::WriteLeadOut => {
            tr!("BURN_WRITE_LEAD_OUT", "Finalising disc")
        }
    }
    .to_string()
}
