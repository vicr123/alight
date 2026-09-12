pub mod blank;

use crate::blank::blank;
use alight::driver::mmc::MmcDriver;
use alight::driver::{BlankMode, CdrDriver, CdrStatusResult, GenericProgress};
use alight::scsi::ScsiError;
use clap::Parser;
use clap_verbosity_flag::InfoLevel;
use cntp_i18n::{tr_info, tr_load, I18N_MANAGER, trn_info};
use indicatif::ProgressBar;
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
    Erase(EraseArgs),
}

#[derive(Parser, Debug)]
pub struct EraseArgs {
    #[arg(long)]
    full: bool,
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
            error!("Failed to create MMC driver: {e:?}");
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
    tr_info!("PAUSE_BEFORE_OPERATION_COMPLETE_STATEMENT", "Starting operation");
}