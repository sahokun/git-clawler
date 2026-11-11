//! Isolate synchronous fetch and credential helpers in a disposable process.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WORKER_ARGUMENT: &str = "--git-clawler-internal-fetch";
#[cfg(not(test))]
const FETCH_BUDGET: Duration = Duration::from_secs(110);

/// Handle the private worker protocol before starting any application threads.
/// Only the application's own binaries call this entry point.
pub fn run_if_requested() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != WORKER_ARGUMENT) {
        return;
    }
    #[cfg(windows)]
    {
        // The parent assigns our Job Object before releasing this gate. No
        // credential helper can escape into the spawn/assignment interval.
        use std::io::Read;
        let mut gate = [0];
        if std::io::stdin().read_exact(&mut gate).is_err() || gate != [1] {
            std::process::exit(1);
        }
    }
    // SAFETY: worker dispatch runs before application or watchdog threads.
    if unsafe { crate::status::configure_network_timeouts() }.is_err() {
        std::process::exit(1);
    }
    #[cfg(unix)]
    let _completion = {
        // A launcher can exit before it can enforce its deadline. Keep a
        // worker-local deadline so its process group cannot remain orphaned.
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            if receiver.recv_timeout(Duration::from_secs(105)).is_err() {
                // Disconnect means normal completion; only a timeout kills.
                if matches!(
                    receiver.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    unsafe extern "C" {
                        fn kill(pid: i32, signal: i32) -> i32;
                    }
                    if let Ok(pid) = i32::try_from(std::process::id()) {
                        // SAFETY: the launcher creates this worker's process
                        // group with id == worker pid; kill only that group.
                        unsafe {
                            kill(-pid, 9);
                        }
                    }
                }
            }
        });
        sender
    };
    let result = if args.len() == 3 {
        git2::Repository::open(Path::new(&args[2]))
            .and_then(|repo| crate::status::fetch_current_upstream_in_worker(&repo))
    } else {
        Err(git2::Error::from_str("invalid internal fetch arguments"))
    };
    // Only fixed exit codes cross the worker boundary. Never send a library
    // message or credential-helper output (which can contain credentials).
    let code = match result {
        Ok(()) => 0,
        Err(ref error) if error.code() == git2::ErrorCode::Auth => 2,
        Err(ref error) if error.code() == git2::ErrorCode::Certificate => 3,
        Err(ref error) if error.class() == git2::ErrorClass::Net => 4,
        Err(ref error) if error.class() == git2::ErrorClass::Ssh => 5,
        Err(ref error) if error.class() == git2::ErrorClass::Ssl => 6,
        Err(_) => 1,
    };
    std::process::exit(code);
}

#[cfg(not(test))]
pub(crate) fn fetch(
    repo: &git2::Repository,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), git2::Error> {
    let executable = std::env::current_exe().map_err(io_error)?;
    let mut command = Command::new(executable);
    command
        .arg(WORKER_ARGUMENT)
        .arg(repo.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000).stdin(Stdio::piped());
    }
    let mut child = command.spawn().map_err(io_error)?;
    #[cfg(windows)]
    let _job = match job_for_child(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = terminate_tree(&mut child);
            return Err(error);
        }
    };
    #[cfg(windows)]
    {
        use std::io::Write;
        if let Err(error) = child.stdin.take().expect("worker gate").write_all(&[1]) {
            let _ = terminate_tree(&mut child);
            return Err(io_error(error));
        }
    }
    wait_cancellable(&mut child, FETCH_BUDGET, cancelled)
}

#[cfg(windows)]
fn job_for_child(child: &Child) -> Result<std::os::windows::io::OwnedHandle, git2::Error> {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    #[repr(C)]
    struct BasicLimits {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            info: *const c_void,
            size: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    }
    // SAFETY: null pointers request an unnamed, non-inheritable job. The
    // repr(C) layout matches JOBOBJECT_EXTENDED_LIMIT_INFORMATION on x86_64.
    unsafe {
        let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if raw.is_null() {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        let job = OwnedHandle::from_raw_handle(raw);
        let limits = ExtendedLimits {
            basic: BasicLimits {
                process_time: 0,
                job_time: 0,
                flags: 0x2000,
                min_working_set: 0,
                max_working_set: 0,
                active_processes: 0,
                affinity: 0,
                priority: 0,
                scheduling: 0,
            },
            io: [0; 6],
            process_memory: 0,
            job_memory: 0,
            peak_process_memory: 0,
            peak_job_memory: 0,
        };
        if SetInformationJobObject(
            job.as_raw_handle(),
            9,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<ExtendedLimits>() as u32,
        ) == 0
            || AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) == 0
        {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        // Closing this handle (including launcher exit) terminates the worker
        // and its credential helper descendants.
        Ok(job)
    }
}

fn io_error(error: std::io::Error) -> git2::Error {
    git2::Error::from_str(&format!("fetch worker process failed: {error}"))
}

#[cfg(test)]
fn wait_bounded(child: &mut Child, budget: Duration) -> Result<(), git2::Error> {
    wait_cancellable(child, budget, &std::sync::atomic::AtomicBool::new(false))
}

fn wait_cancellable(
    child: &mut Child,
    budget: Duration,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<(), git2::Error> {
    let deadline = Instant::now() + budget;
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            terminate_tree(child)?;
            return Err(git2::Error::from_str("fetch cancelled"));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let cause = match status.code() {
                    Some(2) => "fetch authentication failed",
                    Some(3) => "fetch certificate or SSH host-key verification failed",
                    Some(4) => "fetch connection failed",
                    Some(5) => "SSH fetch failed",
                    Some(6) => "HTTPS fetch failed",
                    _ => "fetch failed",
                };
                return Err(git2::Error::from_str(&format!(
                    "{cause}; using cached remote refs"
                )));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                terminate_tree(child)?;
                return Err(git2::Error::from_str(
                    "fetch timed out; using cached remote refs",
                ));
            }
            Err(error) => {
                let _ = terminate_tree(child);
                return Err(io_error(error));
            }
        }
    }
}

fn terminate_tree(child: &mut Child) -> Result<(), git2::Error> {
    #[cfg(unix)]
    {
        // The worker starts its own process group. Credential helpers inherit
        // that group, so terminate them together without touching other apps.
        unsafe extern "C" {
            fn kill(pid: std::os::raw::c_int, signal: std::os::raw::c_int) -> std::os::raw::c_int;
        }
        let group =
            i32::try_from(child.id()).map_err(|_| git2::Error::from_str("invalid worker pid"))?;
        // SAFETY: POSIX kill takes a signed pid and signal. Negative pid targets
        // only the process group established above; 9 is SIGKILL on Linux.
        if unsafe { kill(-group, 9) } != 0 {
            let error = std::io::Error::last_os_error();
            // The worker may have exited between try_wait and kill.
            if child.try_wait().map_err(io_error)?.is_none() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io_error(error));
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let result = Command::new("taskkill.exe")
            .args(["/F", "/T", "/PID"])
            .arg(child.id().to_string())
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(io_error)?;
        if !result.success() && child.try_wait().map_err(io_error)?.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(git2::Error::from_str(
                "failed to terminate fetch process tree",
            ));
        }
    }
    child.wait().map_err(io_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn closing_launcher_job_terminates_worker_tree() {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "ping -n 30 127.0.0.1"])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let job = job_for_child(&child).unwrap();
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "job must terminate the worker");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_worker_and_descendant() {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = child.stdout.take().unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancelled.store(true, std::sync::atomic::Ordering::Release);
            });
            assert!(
                wait_cancellable(&mut child, Duration::from_secs(30), &cancelled)
                    .unwrap_err()
                    .message()
                    .contains("cancelled")
            );
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            output.read_to_end(&mut bytes).unwrap();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("descendant must exit");
    }

    #[test]
    fn stalled_worker_and_credential_child_are_terminated() {
        #[cfg(unix)]
        let mut command = {
            use std::os::unix::process::CommandExt;
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 30 & wait"]).process_group(0);
            command
        };
        #[cfg(windows)]
        let mut command = {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new("cmd.exe");
            command
                .args(["/C", "ping -n 30 127.0.0.1"])
                .creation_flags(0x0800_0000);
            command
        };
        // The descendant keeps the pipe open until it is killed. Reading EOF
        // proves the whole group was terminated, not just the shell launcher.
        command.stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut output = child.stdout.take().unwrap();
        assert!(wait_bounded(&mut child, Duration::from_millis(50))
            .unwrap_err()
            .message()
            .contains("timed out"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            let _ = output.read_to_end(&mut bytes);
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("descendant must release stdout");
    }
}
