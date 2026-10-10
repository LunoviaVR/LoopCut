//! Killing, cancelling and timing out real FFmpeg children. A long realtime source keeps them running until stopped.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use loopcut_media::{CancelToken, FfmpegCommand, FfmpegPaths, InputOptions, MediaError, SpawnIo};

/// A realtime (`-re`) source that would run for ten minutes, discarding its output.
fn endless(paths: &FfmpegPaths) -> FfmpegCommand {
    FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-re")
        .expect("valid FFmpeg argument")
        .input(
            "testsrc2=size=320x240:rate=30:duration=600",
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        )
        .expect("lavfi input")
        .output_args(["-f", "null", "-"])
        .expect("valid FFmpeg argument")
}

#[test]
fn kill_after_200ms_terminates_quickly() {
    let Some(paths) = common::paths() else { return };
    let mut process = endless(&paths).spawn(SpawnIo::default()).expect("spawn");
    thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    process.kill();
    let result = process.wait();
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a killed encode must not report success");
    assert!(elapsed < Duration::from_secs(5), "kill took {elapsed:?}");
}

#[test]
fn cancel_token_stops_a_waiting_process() {
    let Some(paths) = common::paths() else { return };
    let process = endless(&paths).spawn(SpawnIo::default()).expect("spawn");
    let token = CancelToken::new();
    let canceller = token.clone();
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });

    let started = Instant::now();
    let result = process.wait_cancellable(&token, Some(Duration::from_secs(60)));
    let elapsed = started.elapsed();
    trigger.join().expect("trigger thread");

    assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    assert!(elapsed < Duration::from_secs(5), "cancel took {elapsed:?}");
}

#[test]
fn wait_timeout_kills_and_reports_timeout() {
    let Some(paths) = common::paths() else { return };
    let process = endless(&paths).spawn(SpawnIo::default()).expect("spawn");
    let limit = Duration::from_millis(300);

    let started = Instant::now();
    let result = process.wait_timeout(limit);
    let elapsed = started.elapsed();

    assert!(
        matches!(result, Err(MediaError::Timeout(d)) if d == limit),
        "{result:?}"
    );
    assert!(elapsed < Duration::from_secs(5), "timeout took {elapsed:?}");
}

#[test]
fn dropping_a_running_process_kills_it() {
    let Some(paths) = common::paths() else { return };
    let process = endless(&paths).spawn(SpawnIo::default()).expect("spawn");
    let pid = process.id();
    assert!(
        common::pid_alive(pid),
        "child should be running before the drop"
    );
    drop(process);
    assert!(
        common::wait_for_exit(pid, Duration::from_secs(10)),
        "dropping the handle must terminate child {pid}"
    );
}
