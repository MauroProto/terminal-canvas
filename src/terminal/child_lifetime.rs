//! Only this worker owns the child. Termination never runs under daemon locks.
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

trait PhysicalChild: Send {
    fn exited(&mut self) -> io::Result<bool>;
    fn terminate(&mut self) -> io::Result<()>;
    fn wait_confirmed(&mut self) -> io::Result<()>;
}

struct PortableChild(Box<dyn portable_pty::Child + Send + Sync>);
impl PhysicalChild for PortableChild {
    fn exited(&mut self) -> io::Result<bool> {
        #[cfg(windows)]
        {
            let handle = self.0.as_raw_handle().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "child process handle missing")
            })?;
            #[link(name = "kernel32")]
            extern "system" {
                fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
            }
            // SAFETY: the owning child retains this process handle for the call.
            match unsafe { WaitForSingleObject(handle, 0) } {
                0 => Ok(true),
                258 => Ok(false),
                u32::MAX => Err(io::Error::last_os_error()),
                result => Err(io::Error::other(format!(
                    "unconfirmed process poll: {result}"
                ))),
            }
        }
        #[cfg(not(windows))]
        {
            self.0.try_wait().map(|status| status.is_some())
        }
    }
    fn terminate(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            let handle = self.0.as_raw_handle().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "child process handle missing")
            })?;
            #[link(name = "kernel32")]
            extern "system" {
                fn TerminateProcess(process: *mut std::ffi::c_void, code: u32) -> i32;
            }
            // SAFETY: the child retains a valid owned process handle here.
            if unsafe { TerminateProcess(handle, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            self.0.kill()
        }
    }
    fn wait_confirmed(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            // portable-pty 0.9 WinChild::wait ignores the native wait result.
            // The owning child keeps this handle alive across the native wait.
            let handle = self.0.as_raw_handle().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "child process handle missing")
            })?;
            #[link(name = "kernel32")]
            extern "system" {
                fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
            }
            // SAFETY: the owning child keeps this process handle alive through wait.
            match unsafe { WaitForSingleObject(handle, u32::MAX) } {
                0 => Ok(()),
                u32::MAX => Err(io::Error::last_os_error()),
                result => Err(io::Error::other(format!(
                    "unconfirmed process wait: {result}"
                ))),
            }
        }
        // The Windows dependency's wait/try_wait duplicate a handle with
        // unwrap under the process mutex. Native OBJECT_0 is the proof; never
        // re-enter those paths and risk a second panic in the unwind guard.
        #[cfg(not(windows))]
        {
            self.0.wait().map(|_| ())
        }
    }
}

struct ChildOwner(Option<Box<dyn PhysicalChild>>);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.terminate();
            // An unwound/setup-failed factory must not release a live child.
            // A failed wait is retained by retrying here; it never grants proof.
            while child.wait_confirmed().is_err() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

pub(super) struct ChildLifetime {
    terminate: mpsc::Sender<()>,
    pub(super) reaped: Arc<AtomicBool>,
}
impl ChildLifetime {
    pub(super) fn spawn_with(
        spawn: impl FnOnce() -> anyhow::Result<Box<dyn portable_pty::Child + Send + Sync>>,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_with(
            || {
                spawn()
                    .map(|child| Box::new(PortableChild(child)) as Box<dyn PhysicalChild>)
                    .map_err(|error| io::Error::other(error.to_string()))
            },
            on_exit,
        )
    }

    fn start_with(
        spawn: impl FnOnce() -> io::Result<Box<dyn PhysicalChild>>,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        let (terminate, receiver) = mpsc::channel();
        let (ready, admission) = mpsc::channel();
        let reaped = Arc::new(AtomicBool::new(false));
        let proof = reaped.clone();
        let owner = Arc::new(Mutex::new(ChildOwner(None)));
        let worker_owner = owner.clone();
        // Create the owner worker BEFORE the child factory. Worker-start failure
        // therefore has no child to kill or synchronously wait under host locks.
        std::thread::Builder::new()
            .name("pty-child-lifetime".to_owned())
            .spawn(move || {
                if admission.recv().is_err() {
                    return;
                }
                let mut owner = worker_owner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                run_child_owner(
                    owner.0.as_mut().expect("child retained").as_mut(),
                    receiver,
                    &proof,
                );
                owner.0.take();
                drop(owner);
                on_exit();
            })?;
        let child = spawn()?;
        owner.lock().unwrap_or_else(|error| error.into_inner()).0 = Some(child);
        ready
            .send(())
            .map_err(|_| io::Error::other("child owner admission unavailable"))?;
        Ok(Self { terminate, reaped })
    }
    pub(super) fn terminate(&self) {
        let _ = self.terminate.send(());
    }

    #[cfg(test)]
    pub(super) fn for_wait_boundary(
        events: mpsc::Sender<&'static str>,
        wait: mpsc::Receiver<io::Result<()>>,
    ) -> Self {
        struct InjectedChild {
            events: mpsc::Sender<&'static str>,
            wait: mpsc::Receiver<io::Result<()>>,
        }
        impl PhysicalChild for InjectedChild {
            fn exited(&mut self) -> io::Result<bool> {
                Ok(false)
            }
            fn terminate(&mut self) -> io::Result<()> {
                self.events.send("terminate").unwrap();
                Ok(())
            }
            fn wait_confirmed(&mut self) -> io::Result<()> {
                self.events.send("wait").unwrap();
                self.wait.recv().unwrap()
            }
        }
        Self::start_with(|| Ok(Box::new(InjectedChild { events, wait })), || {}).unwrap()
    }
}
impl Drop for ChildLifetime {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn run_child_owner(
    child: &mut dyn PhysicalChild,
    receiver: mpsc::Receiver<()>,
    reaped: &AtomicBool,
) {
    let mut terminating = false;
    loop {
        if terminating {
            let _ = child.terminate();
            if child.wait_confirmed().is_ok() {
                reaped.store(true, Ordering::Release);
                return;
            }
        } else if matches!(child.exited(), Ok(true)) && child.wait_confirmed().is_ok() {
            reaped.store(true, Ordering::Release);
            return;
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => terminating = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn termination_request_returns_before_actual_wait_and_proof_waits_for_success() {
        struct Child {
            entered: mpsc::Sender<()>,
            wait: mpsc::Receiver<io::Result<()>>,
        }
        impl PhysicalChild for Child {
            fn exited(&mut self) -> io::Result<bool> {
                Ok(false)
            }
            fn terminate(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn wait_confirmed(&mut self) -> io::Result<()> {
                self.entered.send(()).unwrap();
                self.wait.recv().unwrap()
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (wait_tx, wait_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let lifetime = ChildLifetime::start_with(
            || {
                Ok(Box::new(Child {
                    entered: entered_tx,
                    wait: wait_rx,
                }))
            },
            move || {
                exit_tx.send(()).unwrap();
            },
        )
        .unwrap();
        lifetime.terminate();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!lifetime.reaped.load(Ordering::Acquire));
        wait_tx
            .send(Err(io::Error::other("unconfirmed wait")))
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!lifetime.reaped.load(Ordering::Acquire));
        wait_tx.send(Ok(())).unwrap();
        exit_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(lifetime.reaped.load(Ordering::Acquire));
    }
}
