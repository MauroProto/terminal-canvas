//! Worker de HTTP del collab manager: ejecuta los `json_post` fuera del hilo
//! de UI. Los métodos del manager encolan un trabajo con todo el contexto
//! necesario y aplican el resultado cuando llega por polling, igual que el
//! `GitInspector` de orquestación. Sin esto, un broker que no responde
//! congela el render hasta el timeout del cliente HTTP.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use chrono::{DateTime, Utc};

use super::models::{GuestId, InviteCode, JoinRequest};
use super::transport::json_post;

/// Qué operación disparó el POST, con el contexto para aplicar el resultado
/// (o revertir el cambio optimista) en el `CollabManager`.
pub(super) enum CollabHttpOp {
    ApproveJoin {
        guest_id: GuestId,
        /// El pedido sacado de `pending_joins` al encolar; vuelve si falla.
        removed_request: Option<JoinRequest>,
    },
    DenyJoin {
        guest_id: GuestId,
        removed_request: Option<JoinRequest>,
    },
    RotateInvite {
        invite_secret: String,
        invite_expires_at: Option<DateTime<Utc>>,
    },
    JoinSession {
        invite: InviteCode,
        display_name: String,
    },
}

pub(super) struct CollabHttpJob {
    pub url: String,
    pub body: serde_json::Value,
    pub tls_cert_pem: Option<String>,
    /// Sube en cada `stop_session`; los resultados de una generación vieja se
    /// descartan para que una sesión cerrada no mute a la siguiente.
    pub generation: u64,
    pub op: CollabHttpOp,
}

pub(super) struct CollabHttpOutcome {
    pub generation: u64,
    pub op: CollabHttpOp,
    pub result: anyhow::Result<serde_json::Value>,
}

#[derive(Default)]
pub(super) struct CollabHttpWorker {
    channels: Option<(Sender<CollabHttpJob>, Receiver<CollabHttpOutcome>)>,
}

impl CollabHttpWorker {
    pub fn request(&mut self, job: CollabHttpJob) {
        if self.channels.is_none() {
            self.channels = Some(spawn_worker());
        }
        let Some((job_tx, _)) = &self.channels else {
            return;
        };
        if let Err(returned) = job_tx.send(job) {
            let (job_tx, result_rx) = spawn_worker();
            let _ = job_tx.send(returned.0);
            self.channels = Some((job_tx, result_rx));
        }
    }

    pub fn poll(&mut self) -> Vec<CollabHttpOutcome> {
        let Some((_, result_rx)) = &self.channels else {
            return Vec::new();
        };
        let mut outcomes = Vec::new();
        while let Ok(outcome) = result_rx.try_recv() {
            outcomes.push(outcome);
        }
        outcomes
    }
}

fn spawn_worker() -> (Sender<CollabHttpJob>, Receiver<CollabHttpOutcome>) {
    let (job_tx, job_rx) = channel::<CollabHttpJob>();
    let (result_tx, result_rx) = channel::<CollabHttpOutcome>();
    let spawned = thread::Builder::new()
        .name("collab-http".to_owned())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let result = json_post::<_, serde_json::Value>(
                    &job.url,
                    &job.body,
                    job.tls_cert_pem.as_deref(),
                );
                let outcome = CollabHttpOutcome {
                    generation: job.generation,
                    op: job.op,
                    result,
                };
                if result_tx.send(outcome).is_err() {
                    break;
                }
            }
        });
    if let Err(err) = spawned {
        log::error!("failed to spawn collab-http worker: {err}");
    }
    (job_tx, result_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn worker_delivers_error_outcome_for_unreachable_url() {
        let mut worker = CollabHttpWorker::default();
        worker.request(CollabHttpJob {
            // Puerto reservado que rechaza la conexión de inmediato.
            url: "http://127.0.0.1:1/v1/share-sessions".to_owned(),
            body: serde_json::json!({}),
            tls_cert_pem: None,
            generation: 7,
            op: CollabHttpOp::RotateInvite {
                invite_secret: "secret".to_owned(),
                invite_expires_at: None,
            },
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let outcomes = worker.poll();
            if let Some(outcome) = outcomes.into_iter().next() {
                assert_eq!(outcome.generation, 7);
                assert!(outcome.result.is_err());
                assert!(matches!(outcome.op, CollabHttpOp::RotateInvite { .. }));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker never delivered an outcome"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
