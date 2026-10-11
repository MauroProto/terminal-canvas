//! Read admissions and physical EOF are separate from a pause acknowledgement.
use std::io::{self, Read};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const IDLE: u8 = 0;
const READING: u8 = 1;
const PAUSE_REQUESTED: u8 = 2;
const PAUSED: u8 = 3;
const DRAINING: u8 = 4;
const DRAIN_READING: u8 = 5;
const EOF: u8 = 6;
const FAILED: u8 = 7;

#[derive(Default)]
pub(super) struct ReaderRetirement {
    state: AtomicU8,
    wake: (Mutex<()>, Condvar),
}

pub(super) enum ReadCompletion {
    Data,
    Retry,
    Paused,
    Eof,
    Failed(io::Error),
}

impl ReaderRetirement {
    pub(super) fn begin_read(&self) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                IDLE => Some(READING),
                DRAINING => Some(DRAIN_READING),
                _ => None,
            })
            .is_ok()
    }

    pub(super) fn finish_read(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                READING => Some(IDLE),
                PAUSE_REQUESTED => Some(PAUSED),
                DRAIN_READING => Some(DRAINING),
                _ => None,
            });
        self.wake.1.notify_all();
    }

    pub(super) fn request(&self) {
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                IDLE => Some(PAUSED),
                READING => Some(PAUSE_REQUESTED),
                _ => None,
            });
        self.wake.1.notify_all();
    }

    pub(super) fn quiescent(&self) -> bool {
        matches!(self.state.load(Ordering::Acquire), PAUSED | EOF)
    }

    pub(super) fn draining(&self) -> bool {
        matches!(self.state.load(Ordering::Acquire), DRAINING | DRAIN_READING)
    }

    pub(super) fn resume_drain(&self) {
        let _ = self
            .state
            .compare_exchange(PAUSED, DRAINING, Ordering::AcqRel, Ordering::Acquire);
        self.wake.1.notify_all();
    }

    pub(super) fn eof(&self) -> bool {
        self.state.load(Ordering::Acquire) == EOF
    }
    pub(super) fn failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == FAILED
    }
    pub(super) fn fail(&self) {
        self.state.store(FAILED, Ordering::Release);
        self.wake.1.notify_all();
    }

    pub(super) fn wait_retry(&self) {
        let guard = self
            .wake
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        drop(self.wake.1.wait_timeout(guard, Duration::from_millis(20)));
    }

    /// The injected closure is the production raw-retention/parser boundary.
    /// Pause is acknowledged only after a successful read has crossed it.
    pub(super) fn read_and_retain(
        &self,
        reader: &mut dyn Read,
        bytes: &mut [u8],
        retain: impl FnOnce(&[u8]) -> io::Result<()>,
    ) -> ReadCompletion {
        if !self.begin_read() {
            return ReadCompletion::Paused;
        }
        let completion = match reader.read(bytes) {
            Ok(0) => {
                self.state.store(EOF, Ordering::Release);
                ReadCompletion::Eof
            }
            Ok(read) => match retain(&bytes[..read]) {
                Ok(()) => ReadCompletion::Data,
                Err(error) => {
                    self.fail();
                    ReadCompletion::Failed(error)
                }
            },
            Err(error) if cfg!(windows) && error.raw_os_error() == Some(109) => {
                self.state.store(EOF, Ordering::Release);
                ReadCompletion::Eof
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) || cfg!(windows) && error.raw_os_error() == Some(995) =>
            {
                ReadCompletion::Retry
            }
            Err(error) => {
                self.fail();
                ReadCompletion::Failed(error)
            }
        };
        self.finish_read();
        completion
    }
}

/// A readiness notification only schedules another physical read. It never
/// consumes bytes or certifies EOF, including POLLHUP/POLLERR. Keep an owned
/// duplicate so closing or reusing the master's descriptor cannot retarget it.
pub(super) struct ReadinessWaiter {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

impl ReadinessWaiter {
    pub(super) fn new(master: &dyn portable_pty::MasterPty) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let fd = master.as_raw_fd().filter(|fd| *fd >= 0).ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "PTY master has no readable fd")
            })?;
            // SAFETY: the borrowed master keeps fd open until duplication completes.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            Ok(Self {
                fd: borrowed.try_clone_to_owned()?,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = master;
            Ok(Self {})
        }
    }

    pub(super) fn wait(&self, retirement: &ReaderRetirement) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor is initialized and points to exactly one entry;
            // the owned fd stays open throughout this bounded, read-only wait.
            let result = unsafe { libc::poll(&mut descriptor, 1, 20) };
            if (result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
                || descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0
            {
                // Unsupported polling/errors still get the old bounded delay,
                // never a hot retry loop or a fabricated physical completion.
                retirement.wait_retry();
            }
        }
        #[cfg(not(unix))]
        retirement.wait_retry();
    }
}

/// The cloned reader and writer share the master open-file description. Set
/// this before starting either worker; InputWriter retains partial offsets.
#[cfg(unix)]
pub(super) fn prepare_nonblocking(master: &dyn portable_pty::MasterPty) -> io::Result<()> {
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "PTY master has no fd"))?;
    // SAFETY: the borrowed master retains fd; F_GETFL takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd remains borrowed from the master; F_SETFL receives integer flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn prepare_nonblocking(_: &dyn portable_pty::MasterPty) -> io::Result<()> {
    Ok(())
}

/// CancelSync does not itself acknowledge a pause. ERROR_NOT_FOUND is a race
/// before/after admission, so every pump retries until the reader acknowledges.
#[cfg(windows)]
pub(super) fn cancel_blocked_read(thread: &std::thread::JoinHandle<()>) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: the borrowed JoinHandle retains the native thread handle for this call.
    if unsafe { CancelSynchronousIo(thread.as_raw_handle()) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(1168) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(not(windows))]
pub(super) fn cancel_blocked_read(_: &std::thread::JoinHandle<()>) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    #[cfg(windows)]
    #[test]
    fn synchronous_cancel_completion_acknowledges_pause_without_eof() {
        struct Aborted(Arc<ReaderRetirement>);
        impl Read for Aborted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.0.request();
                Err(io::Error::from_raw_os_error(995))
            }
        }
        let control = Arc::new(ReaderRetirement::default());
        assert!(matches!(
            control.read_and_retain(&mut Aborted(control.clone()), &mut [0; 8], |_| panic!(
                "cancel has no bytes"
            )),
            ReadCompletion::Retry
        ));
        assert!(control.quiescent());
        assert!(!control.eof());
        assert!(!control.failed());
        control.resume_drain();
        assert!(matches!(
            control.read_and_retain(&mut &b"tail"[..], &mut [0; 8], |_| Ok(())),
            ReadCompletion::Data
        ));
        assert!(!control.eof());
    }

    #[test]
    fn pause_covers_concurrent_success_before_tail_resume() {
        struct ReadAtBoundary {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Read for ReadAtBoundary {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                bytes[..6].copy_from_slice(b"prefix");
                Ok(6)
            }
        }
        let control = Arc::new(ReaderRetirement::default());
        let runner = control.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (retained_tx, retained_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            runner.read_and_retain(
                &mut ReadAtBoundary {
                    entered: entered_tx,
                    release: release_rx,
                },
                &mut [0; 8],
                |raw| {
                    retained_tx.send(raw.to_vec()).unwrap();
                    Ok(())
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        control.request();
        assert!(!control.quiescent());
        release_tx.send(()).unwrap();
        assert!(matches!(thread.join().unwrap(), ReadCompletion::Data));
        assert_eq!(retained_rx.recv().unwrap(), b"prefix");
        assert!(control.quiescent());
        assert!(!control.eof());
        control.resume_drain();
        assert!(matches!(
            control.read_and_retain(&mut &b"tail"[..], &mut [0; 8], |_| Ok(())),
            ReadCompletion::Data
        ));
        assert!(matches!(
            control.read_and_retain(&mut &b""[..], &mut [0; 8], |_| Ok(())),
            ReadCompletion::Eof
        ));
        assert!(control.eof());
    }

    #[cfg(unix)]
    #[test]
    fn readiness_keeps_bytes_and_hangup_separate_from_physical_eof() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let waiter = ReadinessWaiter {
            fd: reader.try_clone().unwrap().into(),
        };
        let control = ReaderRetirement::default();
        let mut bytes = [0; 8];
        waiter.wait(&control);
        assert!(matches!(
            control.read_and_retain(&mut reader, &mut bytes, |_| panic!("no bytes")),
            ReadCompletion::Retry
        ));
        assert!(!control.eof());

        writer.write_all(b"tail").unwrap();
        waiter.wait(&control);
        let mut retained = Vec::new();
        assert!(matches!(
            control.read_and_retain(&mut reader, &mut bytes, |raw| {
                retained.extend_from_slice(raw);
                Ok(())
            }),
            ReadCompletion::Data
        ));
        assert_eq!(retained, b"tail");
        assert!(!control.eof());

        drop(writer);
        waiter.wait(&control);
        assert!(!control.eof(), "hangup readiness is not a read receipt");
        control.request();
        assert!(matches!(
            control.read_and_retain(&mut reader, &mut bytes, |_| panic!("paused")),
            ReadCompletion::Paused
        ));
        assert!(control.quiescent());
        assert!(!control.eof());
        control.resume_drain();
        assert!(matches!(
            control.read_and_retain(&mut reader, &mut bytes, |_| panic!("EOF has no bytes")),
            ReadCompletion::Eof
        ));
        assert!(control.eof());
    }

    #[test]
    fn aborted_and_failed_reads_do_not_certify_eof() {
        struct ErrorRead(io::Error);
        impl Read for ErrorRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(self.0.kind(), self.0.to_string()))
            }
        }
        let control = ReaderRetirement::default();
        assert!(matches!(
            control.read_and_retain(
                &mut ErrorRead(io::ErrorKind::Interrupted.into()),
                &mut [0; 8],
                |_| panic!("no bytes")
            ),
            ReadCompletion::Retry
        ));
        assert!(!control.eof());
        assert!(matches!(
            control.read_and_retain(
                &mut ErrorRead(io::ErrorKind::Other.into()),
                &mut [0; 8],
                |_| panic!("no bytes")
            ),
            ReadCompletion::Failed(_)
        ));
        assert!(control.failed());
        assert!(!control.eof());
    }
}
