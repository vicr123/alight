pub mod blank;
pub mod simple_audio;

use crate::blank::blank;
use crate::simple_audio::simple_audio;
use alight::driver::mmc::MmcDriver;
use alight::driver::{BlankMode, CdrDriver, CdrStatusResult, GenericProgress};
use alight::scsi::ScsiError;
use clap::Parser;
use clap_verbosity_flag::InfoLevel;
use cntp_i18n::{I18N_MANAGER, tr_error, tr_info, tr_load, trn_info};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use std::fmt::Write;
use std::io::Error;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
struct Args {
    #[arg(required = true)]
    device: String,

    #[command(subcommand)]
    command: Command,

    #[clap(flatten)]
    verbosity: clap_verbosity_flag::Verbosity<InfoLevel>,
}

#[derive(Parser, Debug)]
enum Command {
    Eject,
    CloseTray,
    Unlock,
    Erase(EraseArgs),
    BurnAudio(SimpleAudioArgs),
}

#[derive(Parser, Debug)]
pub struct EraseArgs {
    #[arg(long)]
    full: bool,
}

#[derive(Parser, Debug)]
pub struct SimpleAudioArgs {
    #[arg(long)]
    dry: bool,

    #[arg(required = true)]
    files: Vec<String>,
}

fn main() -> ExitCode {
    I18N_MANAGER.load_source(tr_load!());

    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .with_max_level(args.verbosity.tracing_level())
        .init();

    let mmc = match MmcDriver::new(&args.device) {
        Ok(mmc) => mmc,
        Err(e) => {
            tr_error!(
                "MMC_DRIVER_CREATE_ERROR",
                "Failed to create MMC driver: {{error}}",
                error = e.to_string()
            );
            return ExitCode::FAILURE;
        }
    };

    tr_info!(
        "SELECTED_DEVICE",
        "Selected device: {{vendor}} {{product}} at {{path}}",
        vendor = mmc.scsi_driver().vendor(),
        product = mmc.scsi_driver().product(),
        path = mmc.scsi_driver().device()
    );

    match args.command {
        Command::Erase(args) => blank(args, mmc),
        Command::BurnAudio(args) => simple_audio(args, mmc),
        Command::Unlock => {
            if let Err(e) = mmc.unlock_media() {
                tr_error!(
                    "UNLOCK_ERROR",
                    "Unable to unlock media: {{error}}",
                    error = e.to_string()
                );
                return ExitCode::FAILURE;
            };

            ExitCode::SUCCESS
        }
        Command::Eject => {
            if let Err(e) = mmc.eject() {
                tr_error!(
                    "EJECT_ERROR",
                    "Unable to eject media: {{error}}",
                    error = e.to_string()
                );
                return ExitCode::FAILURE;
            };

            ExitCode::SUCCESS
        }
        Command::CloseTray => {
            if let Err(e) = mmc.close_tray() {
                tr_error!(
                    "CLOSE_TRAY_ERROR",
                    "Unable to close tray: {{error}}",
                    error = e.to_string()
                );
                return ExitCode::FAILURE;
            };

            ExitCode::SUCCESS
        }
    }
}

pub fn pause_before_operation() {
    for i in (1..=3).rev() {
        trn_info!(
            "PAUSE_BEFORE_OPERATION_STATEMENT",
            "Starting in {{count}} second. CTRL+C to stop.",
            "Starting in {{count}} seconds. CTRL+C to stop.",
            count = i
        );
        thread::sleep(Duration::from_secs(1));
    }
    tr_info!(
        "PAUSE_BEFORE_OPERATION_COMPLETE_STATEMENT",
        "Starting operation"
    );
}

pub fn progress_style(message: String, working: bool, indeterminate: bool) -> ProgressStyle {
    ProgressStyle::with_template(&format!(
        "{} {{msg}} {}",
        if working { "{spinner}" } else { " " },
        if indeterminate {
            ""
        } else {
            "[{wide_bar}] {percentage}"
        }
    ))
    .unwrap()
    .with_key("msg", move |_: &ProgressState, w: &mut dyn Write| {
        write!(w, "{}", message).unwrap()
    })
    .with_key("percentage", |state: &ProgressState, w: &mut dyn Write| {
        write!(
            w,
            "{:>3.0}%",
            state.pos() as f64 / state.len().unwrap_or(1) as f64 * 100.0
        )
        .unwrap()
    })
    .progress_chars("#-")
}
