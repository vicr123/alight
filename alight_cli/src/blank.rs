use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use cntp_i18n::{tr, tr_info, tr_warn};
use indicatif::ProgressBar;
use tracing::{error, info, warn};
use alight::driver::{BlankMode, CdrDriver, CdrStatusResult};
use alight::driver::mmc::MmcDriver;
use crate::{pause_before_operation, EraseArgs};

pub fn blank(args: EraseArgs, mmc: MmcDriver) -> ExitCode {
    let blank_mode = if args.full {
        BlankMode::Full
    } else {
        BlankMode::Fast
    };

    tr_info!(
        "BLANKING_START_STATEMENT",
        "Erasing media in device {{device}} in {{mode}} mode.",
        device = mmc.scsi_driver().device(),
        mode = match blank_mode {
            BlankMode::Full => "FULL",
            BlankMode::Fast => "FAST",
        }
    );

    pause_before_operation();

    loop {
        match mmc.status() {
            Ok(CdrStatusResult::Ready) => {
                break;
            }
            Ok(_) => {
                tr_warn!("DRIVE_NOT_READY", "Drive not ready. Waiting 5 seconds...");
                thread::sleep(Duration::from_secs(5));
            }
            Err(e) => {
                error!("Failed to query drive ready status: {e:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    let mut progress = match mmc.blank(blank_mode) {
        Ok(progress) => {progress}
        Err(e) => {
            error!("Failed to blank device: {e:?}");
            return ExitCode::FAILURE;
        }
    };

    smol::block_on(async move {
        let started = Instant::now();
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.set_message(tr!("BLANK_IN_PROGRESS", "Erasing...").to_string());

        while let Some(Ok(progress)) = progress.next().await {
            bar.set_elapsed(started.elapsed());
            bar.set_length(progress.total);
            bar.set_position(progress.progress);
        }

        bar.finish();
        
        tr_info!("BLANK_SUCCESS", "Erased media.");
    });

    if mmc.scsi_driver().rezero().is_err() {
        tr_warn!("REZERO_FAILED", "Rezero unsuccessful. Kernel may not be aware of old media. You should eject the media and reload it.");
    }

    ExitCode::SUCCESS
}