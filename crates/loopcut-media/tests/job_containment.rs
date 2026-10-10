//! Kill-on-close containment (Windows only).
//!
//! The test re-runs this test binary as an "owner" process. The owner starts two identical endless FFmpeg children:
//!
//! * the **contained** child, started through the runner, so it is in LoopCut's kill-on-close Job Object; and
//! * a **control** child, started with plain `std::process::Command`, so it is not in any job.
//!
//! Then the owner exits without cleanup. The contained child must die and the control must survive. Both children
//! send progress to a file instead of a pipe, so the owner closing its pipes cannot kill either one; the only
//! difference left between them is containment.

#![cfg(windows)]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use loopcut_media::{FfmpegCommand, FfmpegPaths, SpawnIo};

/// Environment marker that switches the re-executed test binary into owner mode.
const OWNER_ENV: &str = "LOOPCUT_JOB_TEST_OWNER";
/// Environment variable carrying the scratch directory for the owner's progress files.
const DIR_ENV: &str = "LOOPCUT_JOB_TEST_DIR";
const TEST_NAME: &str = "child_dies_only_when_its_owner_exits_without_cleanup";

/// Force-kills a process on drop, so a failing assertion does not leave an encoder running.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if common::pid_alive(self.0) {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.0.to_string(), "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[test]
fn child_dies_only_when_its_owner_exits_without_cleanup() {
    if std::env::var_os(OWNER_ENV).is_some() {
        owner_process();
    }
    if common::paths().is_none() {
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch dir");
    let exe = std::env::current_exe().expect("test binary path");
    // The owner's output goes to a file, not a pipe: children inherit the owner's inheritable handles, so a pipe
    // would stay open until the control child exits, and waiting for EOF would block for the child's whole lifetime.
    let report_path = scratch.path().join("owner.out");
    let report = std::fs::File::create(&report_path).expect("report file");
    let status = Command::new(exe)
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .env(OWNER_ENV, "1")
        .env(DIR_ENV, scratch.path())
        .stdin(Stdio::null())
        .stdout(report.try_clone().expect("clone report handle"))
        .stderr(report)
        .status()
        .expect("run owner process");
    let stdout = std::fs::read_to_string(&report_path).expect("owner report");
    let contained = reported_pid(&stdout, "CONTAINED_PID=", &status);
    let control = reported_pid(&stdout, "CONTROL_PID=", &status);
    let _control_cleanup = KillOnDrop(control);
    let _contained_cleanup = KillOnDrop(contained);

    assert!(
        common::wait_for_exit(contained, Duration::from_secs(10)),
        "contained FFmpeg child {contained} survived its owner; kill-on-close is not working"
    );
    assert!(
        common::pid_alive(control),
        "control child {control} also died; the owner may not have exited the way this test assumes"
    );
}

/// Extracts a pid reported by the owner. libtest writes `test name ... ` without a newline, so the report can share
/// a line with it: search for the marker rather than anchoring it.
fn reported_pid(stdout: &str, marker: &str, status: &std::process::ExitStatus) -> u32 {
    stdout
        .split(marker)
        .nth(1)
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| {
            panic!("owner did not report {marker}; status {status:?}, stdout:\n{stdout}")
        })
}

/// The endless, pipe-free encode both children run. Progress goes to `progress_file`, which overrides the builder's
/// `-progress pipe:2` because the last `-progress` option wins.
fn endless_command(paths: &FfmpegPaths, progress_file: &Path) -> FfmpegCommand {
    let progress_url = format!(
        "file:{}",
        progress_file.to_string_lossy().replace('\\', "/")
    );
    FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-re")
        .expect("valid FFmpeg argument")
        .global_args(["-progress", progress_url.as_str()])
        .expect("valid FFmpeg argument")
        .input(
            "testsrc2=size=320x240:rate=30:duration=600",
            &common::lavfi(),
        )
        .expect("lavfi input")
        .output_args(["-f", "null", "-"])
        .expect("valid FFmpeg argument")
}

/// Starts the contained and control children, reports their pids and exits without cleanup. Never returns.
fn owner_process() -> ! {
    let Some(paths) = common::paths() else {
        std::process::exit(2);
    };
    let scratch = PathBuf::from(std::env::var_os(DIR_ENV).expect("scratch dir from parent"));

    // Contained: through the runner, so it is assigned to the kill-on-close job.
    let contained = endless_command(&paths, &scratch.join("contained.log"))
        .spawn(SpawnIo::default())
        .expect("spawn contained child");

    // Control: the same argument list, but with no job object.
    let control = Command::new(&paths.ffmpeg)
        .args(endless_command(&paths, &scratch.join("control.log")).args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn control child");

    println!("CONTAINED_PID={}", contained.id());
    println!("CONTROL_PID={}", control.id());
    std::io::stdout().flush().expect("flush pid report");
    // Skip both destructors: the contained child must be ended by the job object alone, and the control child has no
    // job at all, so it is expected to outlive us.
    std::mem::forget(contained);
    std::mem::forget(control);
    std::process::exit(0);
}
