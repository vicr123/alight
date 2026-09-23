pub mod resampler;

use crate::simple_audio::resampler::Resampler;
use crate::{SimpleAudioArgs, pause_before_operation, progress_style};
use alight::burn_dao_audio_cd_job::{
    BurnDaoAudioCdJob, BurnDaoAudioCdJobProgress, BurnDaoAudioCdJobProgressTask,
    BurnDaoAudioCdTrack,
};
use alight::driver::mmc::MmcDriver;
use cntp_i18n::{tr, tr_error, tr_info, trn_info};
use indicatif::{MultiProgress, ProgressBar};
use ringbuf::LocalRb;
use ringbuf::consumer::Consumer;
use ringbuf::traits::Observer;
use rubato::FixedSync;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use symphonia::core::audio::sample::{Sample, SampleFormat};
use symphonia::core::audio::{Audio, AudioBuffer};
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::default::{get_codecs, get_probe};
use tracing::{Level, error};

pub fn simple_audio(args: SimpleAudioArgs, mmc: MmcDriver) -> ExitCode {
    let files_len = args.files.len();

    let mut job = BurnDaoAudioCdJob::new();
    job.set_dry(args.dry);
    for file in args.files {
        job.push_track(match create_track(file) {
            Ok(track) => track,
            Err(e) => return e,
        })
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

fn create_track(file: String) -> Result<BurnDaoAudioCdTrack, ExitCode> {
    let mut file = match File::open(&file) {
        Ok(file) => file,
        Err(e) => {
            tr_error!(
                "FILE_ERROR",
                "Cannot open file {{file}}: {{error}}",
                file = file,
                error = e.to_string()
            );
            return Err(ExitCode::FAILURE);
        }
    };

    if let Err(e) = file.seek(SeekFrom::Start(44)) {
        tr_error!(
            "SEEK_ERROR",
            "Cannot seek file: {{error}}",
            error = e.to_string()
        );
        return Err(ExitCode::FAILURE);
    }

    let media_source_stream =
        MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let mut format = match get_probe().probe(
        &Hint::new(),
        media_source_stream,
        Default::default(),
        Default::default(),
    ) {
        Ok(format) => format,
        Err(e) => {
            tr_error!(
                "READ_ERROR",
                "Cannot read file: {{error}}",
                error = e.to_string()
            );
            return Err(ExitCode::FAILURE);
        }
    };

    let Some(track) = format.first_track(TrackType::Audio) else {
        tr_error!("READ_ERROR", error = "No audio track found");
        return Err(ExitCode::FAILURE);
    };

    let Some(audio_track_params) = track
        .codec_params
        .as_ref()
        .and_then(|codec_params| codec_params.audio())
    else {
        tr_error!("READ_ERROR", error = "Audio track cannot be decoded");
        return Err(ExitCode::FAILURE);
    };

    let Some(time_base) = track.time_base else {
        tr_error!("READ_ERROR", error = "Track duration cannot be calculated");
        return Err(ExitCode::FAILURE);
    };

    let Some(duration) = track.duration else {
        tr_error!("READ_ERROR", error = "Track duration cannot be calculated");
        return Err(ExitCode::FAILURE);
    };

    let Some(duration) = time_base.calc_duration(duration) else {
        tr_error!("READ_ERROR", error = "Track duration cannot be calculated");
        return Err(ExitCode::FAILURE);
    };

    let frames = (duration.as_millis() as f64 * const { 75. / 1000. }) as u32;

    let mut decoder = match get_codecs().make_audio_decoder(audio_track_params, &Default::default())
    {
        Ok(decoder) => decoder,
        Err(e) => {
            tr_error!("READ_ERROR", error = e.to_string());
            return Err(ExitCode::FAILURE);
        }
    };

    let mut buffer = LocalRb::new(10240);
    let mut resampler: Option<Resampler<f32>> = None;

    Ok(BurnDaoAudioCdTrack::new(frames, move |frame| {
        loop {
            if buffer.occupied_len() > frame.len() {
                buffer.pop_slice(frame);
                return Ok(());
            };

            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    if let Some(resampler) = resampler.as_mut() {
                        let mut audio_samples = Vec::new();
                        resampler.flush(&mut audio_samples);

                        for audio_sample in audio_samples.into_iter() {
                            let audio_sample = (audio_sample * i16::MAX as f32) as i16;
                            if buffer.write(&audio_sample.to_le_bytes()).is_err() {
                                return Err("Buffer error".into());
                            }
                        }
                    };

                    if buffer.write(&vec![0_u8; buffer.vacant_len()]).is_err() {
                        return Err("Buffer error".into());
                    };

                    continue;
                }
                _ => return Err("Unable to read next packet".into()),
            };

            let audio = match decoder.decode(&packet) {
                Ok(audio) => audio,
                Err(e) => return Err("Unable to decode next packet".into()),
            };

            if resampler.is_none() {
                _ = resampler.insert(Resampler::new(audio.spec(), 44100, audio.capacity()));
            }

            let resampler = resampler
                .as_mut()
                .expect("The resampler is guaranteed to be created");

            let mut audio_samples = Vec::new();
            resampler.resample(audio, &mut audio_samples);

            for audio_sample in audio_samples.into_iter() {
                let audio_sample = (audio_sample * i16::MAX as f32) as i16;
                if buffer.write(&audio_sample.to_le_bytes()).is_err() {
                    return Err("Buffer error".into());
                }
            }
        }
    }))
}
