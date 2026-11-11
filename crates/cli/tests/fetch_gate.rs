//! The private Windows fetch worker must wait until its parent assigns a Job Object.
#![cfg(windows)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn worker_does_not_touch_git_before_parent_releases_gate() {
    for release in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut worker = Command::new(env!("CARGO_BIN_EXE_gclawl"))
            .arg("--git-clawler-internal-fetch")
            .arg(directory.path().join("absent-repository"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // An ungated worker would already have failed to open this repository.
        std::thread::sleep(Duration::from_secs(1));
        assert!(worker.try_wait().unwrap().is_none());
        let mut gate = worker.stdin.take().unwrap();
        if release {
            gate.write_all(&[1]).unwrap();
        }
        drop(gate); // Parent death/failed assignment also releases the wait safely.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = worker.try_wait().unwrap() {
                assert_eq!(status.code(), Some(1));
                break;
            }
            if Instant::now() >= deadline {
                worker.kill().unwrap();
                worker.wait().unwrap();
                panic!("worker must exit after the startup gate closes");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
