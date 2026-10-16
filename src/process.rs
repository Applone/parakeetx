use std::{io::Read, ops::{Deref, DerefMut}, process::{Child, Command}, sync::atomic::{AtomicBool, Ordering}, thread, time::{Duration, Instant}};

use anyhow::{Context, Result, bail, ensure};

// Own the process tree, including uv's Python installers and build subprocesses.
pub(crate) struct ManagedChild {
    child: Child,
    stopped: bool,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl ManagedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        let child = command.spawn().context("Cannot start subprocess")?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            let mut child = child;
            // SAFETY: All pointers refer to initialized structures for the duration
            // of the calls. This object exclusively owns the returned job handle.
            let job = unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if !job.is_null() {
                    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as _, std::mem::size_of_val(&limits) as u32) == 0
                        || AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                        CloseHandle(job);
                        std::ptr::null_mut()
                    } else { job }
                } else { job }
            };
            if job.is_null() {
                let error = std::io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("Cannot isolate subprocess tree");
            }
            Ok(Self { child, job, stopped: false })
        }
        #[cfg(not(windows))]
        Ok(Self { child, stopped: false })
    }

    pub fn stop_tree(&mut self) {
        if self.stopped { return; }
        self.stopped = true;
        #[cfg(unix)]
        // SAFETY: spawn creates a separate process group with the child's PID.
        // A negative PID targets that group, never the application's group.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL); }
        #[cfg(windows)]
        // SAFETY: The job handle is owned by this instance and still open.
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job, 1); }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child { &self.child }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child { &mut self.child }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.stop_tree();
        #[cfg(windows)]
        // SAFETY: This is the sole owner; no later call uses the handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job); }
    }
}

pub(crate) fn run(command: &mut Command, cancel: &AtomicBool, timeout: Duration, stage: &str) -> Result<String> {
    use std::process::Stdio;
    ensure!(!cancel.load(Ordering::Relaxed), "{stage} cancelled");
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ManagedChild::spawn(command).with_context(|| stage.to_owned())?;
    let stdout = child.stdout.take().context("Missing subprocess output")?;
    let stderr = child.stderr.take().context("Missing subprocess diagnostics")?;
    let (sender, receiver) = crossbeam_channel::unbounded();
    for mut pipe in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr) as Box<dyn Read + Send>] {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut tail = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        tail.extend_from_slice(&buffer[..count]);
                        if tail.len() > 16 * 1024 { tail.drain(..tail.len() - 16 * 1024); }
                    }
                }
            }
            let _ = sender.send(tail);
        });
    }
    drop(sender);
    let deadline = Instant::now() + timeout;
    let status = loop {
        ensure!(!cancel.load(Ordering::Relaxed), "{stage} cancelled");
        ensure!(Instant::now() < deadline, "{stage} timed out");
        if let Some(status) = child.try_wait()? { break status; }
        thread::sleep(Duration::from_millis(50));
    };
    child.stop_tree();
    let mut output = String::new();
    for _ in 0..2 {
        let bytes = receiver.recv_timeout(Duration::from_secs(5)).context("Subprocess output pipes did not close")?;
        output.push_str(&String::from_utf8_lossy(&bytes));
        output.push('\n');
    }
    if !status.success() { bail!("{stage} failed ({status}). {}", output.trim()); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python(code: &str) -> Command {
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
        command.args(["-I", "-c", code]);
        command
    }

    #[test]
    fn captures_bounded_diagnostics_from_both_pipes_and_reports_failure() {
        let output = run(&mut python("import sys;sys.stdout.write('a'*200000);sys.stderr.write('b'*200000)"), &AtomicBool::new(false), Duration::from_secs(5), "Install").unwrap();
        assert!(output.len() <= 32 * 1024 + 2);
        assert!(output.contains('a') && output.contains('b'));
        let error = run(&mut python("import sys;sys.stderr.write('installation failed');sys.exit(1)"), &AtomicBool::new(false), Duration::from_secs(5), "Install").unwrap_err();
        assert!(error.to_string().contains("installation failed"));
    }

    #[test]
    fn timeout_and_cancellation_stop_processes_promptly() {
        let started = Instant::now();
        assert!(run(&mut python("import time;time.sleep(60)"), &AtomicBool::new(false), Duration::from_millis(100), "Install").unwrap_err().to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
        let cancel = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| { thread::sleep(Duration::from_millis(100)); cancel.store(true, Ordering::Relaxed); });
            assert!(run(&mut python("import time;time.sleep(60)"), &cancel, Duration::from_secs(90), "Install").unwrap_err().to_string().contains("cancelled"));
        });
    }

    #[test]
    fn cancellation_also_stops_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let marker = directory.path().join("descendant-survived");
        let mut command = python("import pathlib,subprocess,sys,time;subprocess.Popen([sys.executable,'-c',\"import pathlib,sys,time;time.sleep(0.6);pathlib.Path(sys.argv[1]).write_text('alive')\",sys.argv[2]]);pathlib.Path(sys.argv[1]).write_text('ready');time.sleep(60)");
        command.arg(&ready).arg(&marker);
        let cancel = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !ready.exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(10)); }
                cancel.store(true, Ordering::Relaxed);
            });
            assert!(run(&mut command, &cancel, Duration::from_secs(5), "Install").is_err());
        });
        assert!(ready.exists(), "descendant must have started before cancellation");
        thread::sleep(Duration::from_millis(700));
        assert!(!marker.exists(), "the descendant must not survive cancellation");
    }
}
