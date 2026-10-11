//! All journal and credential-file IO stays off the egui thread.
use super::*;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Binding {
    pub conversation: Conversation,
    pub can_deliver: bool,
}
pub enum Job {
    Observe(Vec<Binding>),
    Snapshot(Uuid),
    History(Uuid, i64),
    Message(Uuid),
    Transfer(Uuid),
    Flush(SyncSender<Result<(), String>>),
    Record {
        id: Uuid,
        thread: Uuid,
        text: String,
    },
    Preview(SendRequest),
    Send(Box<PreparedSend>),
    Link(Uuid, Uuid, bool),
    Unlink(Uuid, Uuid),
    Approve(Uuid),
    DeliverNow(Uuid),
    Cancel(Uuid),
    Delivered(Uuid, Result<(), String>),
    Reply(Uuid, String),
    Resolve(Uuid, bool),
    Config(Uuid),
    Revoke(Uuid),
}
pub enum Event {
    Snapshot(Uuid, HubSnapshot),
    History(Uuid, i64, Vec<HistoryEntry>),
    Message(ConversationMessage),
    Transfer(Box<Transfer>),
    Preview(Box<PreparedSend>),
    Saved(Uuid),
    Deliver(Box<Transfer>, Box<Conversation>),
    Config(Uuid, PathBuf),
    Error(String),
    Attention(Uuid),
}
pub struct Worker {
    pub jobs: SyncSender<Job>,
    pub events: Receiver<Event>,
    active: Arc<AtomicBool>,
}
impl Worker {
    pub fn start(database: PathBuf, ctx: egui::Context) -> std::io::Result<Self> {
        let (jobs, rx) = mpsc::sync_channel(64);
        let (tx, events) = mpsc::sync_channel(64);
        let active = Arc::new(AtomicBool::new(true));
        let enabled = active.clone();
        std::thread::Builder::new()
            .name("conversation-journal".into())
            .spawn(move || {
                let send = |event| {
                    let result = tx.send(event);
                    ctx.request_repaint();
                    result.is_ok()
                };
                let mut store = match ConversationStore::open(&database) {
                    Ok(store) => store,
                    Err(error) => {
                        send(Event::Error(format!(
                            "Historial de conversaciones: {error}"
                        )));
                        return;
                    }
                };
                let mut bindings = Vec::<Binding>::new();
                let mut attention=std::collections::HashSet::new();
                let mut receipt_errors=std::collections::HashMap::<Uuid,String>::new();
                loop {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(job) => {
                            let result = (|| -> anyhow::Result<Option<Event>> {
                                match job {
                                    Job::Observe(rows) => {
                                        for row in &rows {
                                            store.register(&row.conversation)?;
                                        }
                                        bindings = rows;
                                        Ok(None)
                                    }
                                    Job::Snapshot(id) => Ok(Some(Event::Snapshot(
                                        id,
                                        store.snapshot(Caller::User, id)?,
                                    ))),
                                    Job::History(id, after) => Ok(Some(Event::History(
                                        id,
                                        after,
                                        store.history(Caller::User, id, after)?,
                                    ))),
                                    Job::Transfer(id) => Ok(Some(Event::Transfer(Box::new(
                                        store.transfer(Caller::User, id)?,
                                    )))),
                                    Job::Flush(ack) => {
                                        let result=receipt_errors.values().next().map_or(Ok(()),|error|Err(error.clone()));
                                        let _ = ack.send(result);
                                        Ok(None)
                                    }
                                    Job::Message(id) => Ok(Some(Event::Message(
                                        store.message(Caller::User, id)?.ok_or_else(|| {
                                            anyhow::anyhow!("Mensaje inexistente")
                                        })?,
                                    ))),
                                    Job::Record { id, thread, text } => {
                                        store.record(
                                            Caller::User,
                                            id,
                                            thread,
                                            MessageRole::Context,
                                            &text,
                                            "terminal_selection",
                                        )?;
                                        Ok(Some(Event::Saved(id)))
                                    }
                                    Job::Preview(request) => {
                                        let context = store.context(
                                            Caller::User,
                                            request.source_id,
                                            &request.message_ids,
                                            request.context_budget_bytes,
                                        )?;
                                        let target_binding = store
                                            .authorize(Caller::User, request.target_id)?
                                            .binding_id;
                                        Ok(Some(Event::Preview(Box::new(PreparedSend {
                                            request,
                                            context,
                                            target_binding,
                                        }))))
                                    }
                                    Job::Send(prepared) => {
                                        store.send_prepared(Caller::User, &prepared)?;
                                        Ok(Some(Event::Saved(prepared.request.operation_id)))
                                    }
                                    Job::Link(a, b, automatic) => {
                                        store.link_pair(a, b, automatic)?;
                                        Ok(None)
                                    }
                                    Job::Unlink(a, b) => {
                                        store.unlink(a, b)?;
                                        Ok(None)
                                    }
                                    Job::Approve(id) => {
                                        store.approve(id)?;
                                        Ok(None)
                                    }
                                    Job::DeliverNow(id)=>{
                                        let transfer=store.transfer(Caller::User,id)?;
                                        let target=bindings.iter().find(|b|b.conversation.id==transfer.target_id).ok_or_else(||anyhow::anyhow!("El destino no está conectado en esta ventana"))?.conversation.clone();
                                        let generation=target.binding_id.ok_or_else(||anyhow::anyhow!("El destino no tiene una sesión activa"))?;
                                        let transfer=store.claim(id,generation)?.ok_or_else(||anyhow::anyhow!("La operación todavía no puede entregarse; revisá MCP, la sesión y los mensajes anteriores"))?;
                                        Ok(Some(Event::Deliver(Box::new(transfer),Box::new(target))))
                                    }
                                    Job::Cancel(id) => {
                                        store.cancel(Caller::User, id)?;
                                        Ok(None)
                                    }
                                    Job::Delivered(id, result) => {
                                        if let Err(error)=store.delivery_result(id,result){receipt_errors.insert(id,error.to_string());return Err(error)}
                                        receipt_errors.remove(&id);
                                        Ok(None)
                                    }
                                    Job::Reply(id, text) => {
                                        store.reply(Caller::User, id, &text)?;
                                        Ok(Some(Event::Saved(id)))
                                    }
                                    Job::Resolve(id, delivered) => {
                                        store.delivery_result(
                                            id,
                                            if delivered {
                                                Ok(())
                                            } else {
                                                Err("El usuario confirmó que no se entregó".into())
                                            },
                                        )?;
                                        receipt_errors.remove(&id);
                                        Ok(None)
                                    }
                                    Job::Config(id) => {
                                        let config = launch::issue_config(
                                            &store,
                                            id,
                                            &database,
                                            &database
                                                .parent()
                                                .ok_or_else(|| {
                                                    anyhow::anyhow!("Perfil sin directorio")
                                                })?
                                                .join("chat-mcp"),
                                            &std::env::current_exe()?,
                                        )?;
                                        Ok(Some(Event::Config(id, config.path)))
                                    }
                                    Job::Revoke(id) => {
                                        store.revoke(id)?;
                                        Ok(None)
                                    }
                                }
                            })();
                            match result {
                                Ok(Some(event)) => {
                                    if !send(event) {
                                        return;
                                    }
                                }
                                Err(error) if !send(Event::Error(error.to_string())) => return,
                                _ => {}
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if !enabled.load(Ordering::Acquire) {
                        continue;
                    }
                    let targets=bindings.iter().map(|binding|binding.conversation.id).collect::<Vec<_>>();
                    if let Ok(current)=store.attention(&targets) {
                        let current=current.into_iter().collect::<std::collections::HashSet<_>>();
                        for id in current.difference(&attention) {if !send(Event::Attention(*id)){return}}
                        attention=current;
                    }
                    for binding in &bindings {
                        if !store.bridge_availability(binding.conversation.id).unwrap_or(Some(false)).unwrap_or(binding.can_deliver) {
                            continue;
                        }
                        let target = &binding.conversation;
                        let Some(generation) = target.binding_id else {
                            continue;
                        };
                        match store.next_pending(target.id) {
                            Ok(Some(id)) => match store.claim(id, generation) {
                                Ok(Some(transfer)) => {
                                    if !send(Event::Deliver(
                                        Box::new(transfer),
                                        Box::new(target.clone()),
                                    )) {
                                        return;
                                    }
                                }
                                Err(error) if !send(Event::Error(error.to_string())) => return,
                                _ => {}
                            },
                            Err(error) if !send(Event::Error(error.to_string())) => return,
                            _ => {}
                        }
                    }
                }
            })?;
        Ok(Self {
            jobs,
            events,
            active,
        })
    }
    pub fn active(&self, value: bool) {
        self.active.store(value, Ordering::Release)
    }
    pub fn submit(&self, job: Job) -> Result<(), String> {
        self.jobs.try_send(job).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => {
                "El historial está ocupado; esperá y volvé a intentar".into()
            }
            mpsc::TrySendError::Disconnected(_) => {
                "El worker de conversaciones se detuvo; reiniciá la app".into()
            }
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.active(false);
    }
}
