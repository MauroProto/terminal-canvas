//! Acceptance by the terminal input queue is distinct from agent execution.
//!
//! A deferred prompt has a receipt so its caller can keep the original draft
//! until the runtime accepts it, or report why it could not be submitted.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub enum PromptSubmission {
    InputQueued,
    Deferred(DeferredPrompt),
}

pub struct DeferredPrompt {
    result: Receiver<io::Result<()>>,
    cancelled: Arc<AtomicBool>,
    cancel_on_drop: bool,
}

impl DeferredPrompt {
    pub(super) fn channel() -> (Sender<io::Result<()>>, Self) {
        let (sender, result) = mpsc::channel();
        (
            sender,
            Self {
                result,
                cancelled: Arc::new(AtomicBool::new(false)),
                cancel_on_drop: true,
            },
        )
    }

    pub(super) fn cancellation_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Legacy fire-and-forget callers explicitly retain their old lifetime.
    pub(crate) fn detach(mut self) {
        self.cancel_on_drop = false;
    }

    /// `None` means the startup gate or input capacity still blocks delivery.
    /// Success means input was queued, not that the agent read or ran it.
    /// Consume the result once and discard the receipt after `Some`.
    pub fn poll(&self) -> Option<io::Result<()>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "El terminal se cerró antes de aceptar el prompt pendiente.",
            ))),
        }
    }
}

impl Drop for DeferredPrompt {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeferredPrompt;
    use std::io;

    #[test]
    fn deferred_receipt_waits_for_input_acceptance() {
        let (sender, receipt) = DeferredPrompt::channel();
        assert!(receipt.poll().is_none());
        sender.send(Ok(())).unwrap();
        assert!(receipt.poll().unwrap().is_ok());
    }

    #[test]
    fn deferred_receipt_preserves_delivery_failure() {
        let (sender, receipt) = DeferredPrompt::channel();
        sender
            .send(Err(io::Error::new(io::ErrorKind::WouldBlock, "input full")))
            .unwrap();
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn dropping_a_pending_session_reports_failure_instead_of_success() {
        let (sender, receipt) = DeferredPrompt::channel();
        drop(sender);
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
