//! `job::terminate_all` kills every child in the process-wide Job Object, so it must not share a test process with
//! tests that run FFmpeg children concurrently. Integration tests run as their own process, which isolates it.

#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use loopcut_media::job::{assign, terminate_all};

/// Starts a long-running `ping` whose output goes nowhere.
fn long_child() -> Child {
    Command::new("ping")
        .args(["-n", "120", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start ping")
}

fn wait_for_exit(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn terminate_all_kills_children_and_the_next_child_is_contained_again() {
    let mut first = long_child();
    assign(&first).expect("assign first child");
    terminate_all().expect("terminate the job");
    let status = wait_for_exit(&mut first).expect("the terminated child must exit");
    assert!(
        !status.success(),
        "a terminated child must not report success"
    );

    // The job was replaced, so a child assigned afterwards is contained by the new job and can be terminated.
    let mut second = long_child();
    assign(&second).expect("assign to the replacement job");
    terminate_all().expect("terminate the replacement job");
    assert!(
        wait_for_exit(&mut second).is_some(),
        "the child in the replacement job must exit"
    );
}
