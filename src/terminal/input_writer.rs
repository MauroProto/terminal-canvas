//! Bounded, nonblocking input handoff. Only the worker touches the OS writer.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};

pub(crate) const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct State {
    pending: VecDeque<Vec<u8>>,
    buffered: usize,
    closed: bool,
    finishing: bool,
    error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct InputWriter {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Drop for InputWriter {
    fn drop(&mut self) {
        // One reference belongs to the worker itself.
        if Arc::strong_count(&self.shared) <= 2 {
            let finishing = self
                .shared
                .0
                .lock()
                .map(|state| state.finishing)
                .unwrap_or(false);
            if !finishing {
                self.close();
            }
        }
    }
}

impl InputWriter {
    pub fn new(mut writer: Box<dyn Write + Send>) -> io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = Arc::clone(&shared);
        std::thread::Builder::new().name("terminal-input".to_owned()).spawn(move || {
            loop {
                let (state, ready) = &*worker;
                let mut guard = state.lock().unwrap_or_else(|error| error.into_inner());
                while guard.pending.is_empty() && !guard.closed && !guard.finishing {
                    guard = ready.wait(guard).unwrap_or_else(|error| error.into_inner());
                }
                if guard.closed || (guard.finishing && guard.pending.is_empty()) { return; }
                let bytes = guard.pending.pop_front().expect("pending input");
                drop(guard);
                let result = writer.write_all(&bytes).and_then(|()| writer.flush());
                let mut guard = state.lock().unwrap_or_else(|error| error.into_inner());
                guard.buffered = guard.buffered.saturating_sub(bytes.len());
                if let Err(error) = result {
                    guard.error = Some(format!("Error de entrada: {error}. Parte del texto puede no haber llegado al proceso."));
                    guard.pending.clear();
                    guard.buffered = 0;
                    guard.closed = true;
                    return;
                }
            }
        })?;
        Ok(Self { shared })
    }

    /// Either accepts the complete input or rejects it visibly. The budget
    /// includes the in-flight write so a blocked child cannot grow memory.
    pub fn enqueue(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let (state, ready) = &*self.shared;
        let mut guard = state
            .lock()
            .map_err(|_| io::Error::other("input queue poisoned"))?;
        let failure = if bytes.len() > MAX_INPUT_BYTES {
            Some((io::ErrorKind::InvalidInput, "No se envió la entrada: el texto supera el límite de 2 MiB. Dividilo en partes más pequeñas."))
        } else if guard.closed || guard.finishing {
            Some((
                io::ErrorKind::BrokenPipe,
                "No se envió la entrada: el canal del terminal está cerrado.",
            ))
        } else if bytes.len() > MAX_INPUT_BYTES.saturating_sub(guard.buffered) {
            Some((io::ErrorKind::WouldBlock, "No se envió la entrada: el proceso no consume datos y la cola de 2 MiB está llena. Reintentá cuando responda."))
        } else {
            None
        };
        if let Some((kind, message)) = failure {
            guard.error = Some(message.to_owned());
            return Err(io::Error::new(kind, message));
        }
        guard.buffered += bytes.len();
        guard.pending.push_back(bytes.to_vec());
        ready.notify_one();
        Ok(())
    }

    pub fn error(&self) -> Option<String> {
        self.shared.0.lock().ok()?.error.clone()
    }

    pub fn record_error(&self, message: String) {
        if let Ok(mut state) = self.shared.0.lock() {
            state.error = Some(message);
        }
    }

    /// Send a final control message after cancelling queued input. It flushes
    /// asynchronously even if the last caller drops immediately afterwards.
    #[cfg(any(test, all(unix, feature = "daemon")))]
    pub fn enqueue_final(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self
            .shared
            .0
            .lock()
            .map_err(|_| io::Error::other("input queue poisoned"))?;
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let queued: usize = state.pending.iter().map(Vec::len).sum();
        state.buffered = state.buffered.saturating_sub(queued) + bytes.len();
        state.pending.clear();
        state.pending.push_back(bytes.to_vec());
        state.finishing = true;
        self.shared.1.notify_one();
        Ok(())
    }

    /// Never joins a writer blocked in an OS call. The owning PTY/socket is
    /// closed separately, which unblocks that call and lets the worker exit.
    pub fn close(&self) {
        if let Ok(mut state) = self.shared.0.lock() {
            state.closed = true;
            state.pending.clear();
            state.buffered = 0;
        }
        self.shared.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct BlockedWriter {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_os_writer_does_not_block_enqueue_rejection_or_close() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = InputWriter::new(Box::new(BlockedWriter {
            entered: entered_tx,
            release: release_rx,
        }))
        .unwrap();
        writer.enqueue(&vec![b'x'; MAX_INPUT_BYTES]).unwrap();
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            writer.enqueue(b"rejected").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(writer.error().unwrap().contains("No se envió"));
        writer.close();
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn reports_os_failures_instead_of_silently_losing_input() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed child"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writer = InputWriter::new(Box::new(Broken)).unwrap();
        writer.enqueue(b"hello").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while writer.error().is_none() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(writer.error().unwrap().contains("closed child"));
        writer.close();
    }

    #[test]
    fn final_control_message_survives_last_owner_drop() {
        struct Recorder(std::sync::mpsc::Sender<Vec<u8>>);
        impl Write for Recorder {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = InputWriter::new(Box::new(Recorder(tx))).unwrap();
        writer.record_error("previous rejection".to_owned());
        assert_eq!(writer.error().as_deref(), Some("previous rejection"));
        writer.enqueue_final(b"kill").unwrap();
        drop(writer);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            b"kill"
        );
    }
}
