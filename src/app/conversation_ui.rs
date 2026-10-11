use super::TerminalApp;
use crate::conversation::{
    worker::{Binding, Event, Job, Worker},
    *,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(super) struct ConversationUi {
    worker: Worker,
    pub(super) open: bool,
    workspace: Uuid,
    snapshot: HubSnapshot,
    source: Option<Uuid>,
    target: Option<Uuid>,
    history: Vec<HistoryEntry>,
    history_end: bool,
    selected: HashSet<Uuid>,
    draft: String,
    budget: usize,
    automatic: bool,
    preview: Option<PreparedSend>,
    pending: Option<Uuid>,
    detail: Option<ConversationMessage>,
    operation: Option<Box<Transfer>>,
    reply: String,
    error: Option<String>,
    notice: Option<String>,
    message_offset: usize,
    request_offset: usize,
    result_offset: usize,
    bindings: HashMap<Uuid, (Uuid, Uuid)>,
    receipts: VecDeque<Job>,
    last_poll: Instant,
}
impl ConversationUi {
    fn new(ctx: egui::Context, workspace: Uuid) -> anyhow::Result<Self> {
        Ok(Self::with_worker(
            Worker::start(database_path()?, ctx)?,
            workspace,
        ))
    }
    fn with_worker(worker: Worker, workspace: Uuid) -> Self {
        Self {
            worker,
            open: false,
            workspace,
            snapshot: HubSnapshot {
                conversations: vec![],
                connected: vec![],
                links: vec![],
                transfers: vec![],
            },
            source: None,
            target: None,
            history: vec![],
            history_end: false,
            selected: HashSet::new(),
            draft: String::new(),
            budget: DEFAULT_CONTEXT_BYTES,
            automatic: false,
            preview: None,
            pending: None,
            detail: None,
            operation: None,
            reply: String::new(),
            error: None,
            notice: None,
            message_offset: 0,
            request_offset: 0,
            result_offset: 0,
            bindings: HashMap::new(),
            receipts: VecDeque::new(),
            last_poll: Instant::now() - Duration::from_secs(2),
        }
    }
    fn job(&mut self, job: Job) -> bool {
        match self.worker.submit(job) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
    fn request(&self) -> Option<SendRequest> {
        let (Some(source_id), Some(target_id)) = (self.source, self.target) else {
            return None;
        };
        if source_id == target_id || self.draft.trim().is_empty() {
            return None;
        }
        let message_ids = self
            .history
            .iter()
            .filter(|h| self.selected.contains(&h.id))
            .map(|h| h.id)
            .collect();
        Some(SendRequest {
            operation_id: Uuid::new_v4(),
            source_id,
            target_id,
            text: self.draft.clone(),
            message_ids,
            context_budget_bytes: self.budget,
        })
    }
    fn preview_matches(&self) -> bool {
        self.preview.as_ref().is_some_and(|prepared| {
            let old = &prepared.request;
            self.request().is_some_and(|current| {
                old.source_id == current.source_id
                    && old.target_id == current.target_id
                    && old.text == current.text
                    && old.message_ids == current.message_ids
                    && old.context_budget_bytes == current.context_budget_bytes
            })
        })
    }
    fn consume(&mut self, event: Event) -> Option<(Box<Transfer>, Box<Conversation>)> {
        match event {
            Event::Snapshot(workspace,snapshot) if workspace==self.workspace=>self.snapshot=snapshot,
            Event::History(id,_,rows) if self.source==Some(id)=>{
                self.history_end=rows.len()<50;
                for row in rows {if !self.history.iter().any(|old|old.id==row.id){self.history.push(row)}}
            }
            Event::Message(message)=>{self.message_offset=0;self.detail=Some(message);}
            Event::Transfer(transfer)=>{self.request_offset=0;self.result_offset=0;self.reply=String::new();self.operation=Some(transfer);}
            Event::Preview(prepared)=>self.preview=Some(*prepared),
            Event::Saved(id)=>{
                if self.pending==Some(id){
                    self.pending=None;
                    if self.preview_matches(){self.draft.clear();self.preview=None;}
                    self.notice=Some("Transferencia guardada; su estado aparece abajo".into());
                }
                if let Some(source)=self.source {self.job(Job::History(source,self.history.last().map_or(0,|h|h.sequence)));}
                if self.operation.as_ref().is_some_and(|operation|operation.id==id){self.job(Job::Transfer(id));}
                self.job(Job::Snapshot(self.workspace));
            }
            Event::Config(id,path)=>self.notice=Some(format!("Configuración MCP del chat {id}: {}. Importala en el proveedor. La credencial anterior fue revocada.",path.display())),
            Event::Error(error)=>{self.error=Some(error);self.pending=None;}
            Event::Deliver(transfer,target)=>return Some((transfer,target)),
            _=>{}
        }
        None
    }
    fn drain_shutdown_events(&mut self, failure: &mut Option<String>) {
        // Closing must not generate UI refresh jobs or submit terminal input.
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                Event::Deliver(transfer, _) => self.receipts.push_back(Job::Delivered(
                    transfer.id,
                    Err("Cierre de la app antes de entregar".into()),
                )),
                Event::Error(error) => {
                    failure.get_or_insert(error);
                }
                _ => {}
            }
        }
    }
    fn finish(&mut self) -> anyhow::Result<()> {
        use std::sync::mpsc::{self, TryRecvError, TrySendError};

        self.worker.active(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut ack: Option<mpsc::Receiver<Result<(), String>>> = None;
        let mut receipts_after_barrier = false;
        let mut failure = None;
        loop {
            self.drain_shutdown_events(&mut failure);
            while let Some(job) = self.receipts.pop_front() {
                match self.worker.jobs.try_send(job) {
                    Ok(()) => receipts_after_barrier = true,
                    Err(TrySendError::Full(job)) => {
                        self.receipts.push_front(job);
                        break;
                    }
                    Err(TrySendError::Disconnected(job)) => {
                        self.receipts.push_front(job);
                        anyhow::bail!("El worker se detuvo antes de guardar las conversaciones");
                    }
                }
            }
            if let Some(receiver) = &ack {
                match receiver.try_recv() {
                    Ok(result) => {
                        if let Err(error) = result {
                            failure.get_or_insert(error);
                        }
                        ack = None;
                        // Events preceding this acknowledgement are now visible.
                        // A late Deliver still needs its own durable receipt.
                        self.drain_shutdown_events(&mut failure);
                        if !receipts_after_barrier && self.receipts.is_empty() {
                            return failure.map_or(Ok(()), |error| Err(anyhow::Error::msg(error)));
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        anyhow::bail!("No se confirmó el guardado de las conversaciones");
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if ack.is_none() && self.receipts.is_empty() {
                let (tx, rx) = mpsc::sync_channel(1);
                match self.worker.jobs.try_send(Job::Flush(tx)) {
                    Ok(()) => {
                        ack = Some(rx);
                        receipts_after_barrier = false;
                    }
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => {
                        anyhow::bail!("El worker se detuvo antes de guardar las conversaciones");
                    }
                }
            }
            if Instant::now() >= deadline {
                anyhow::bail!("No se confirmó el guardado de las conversaciones antes del cierre");
            }
            // Keep draining bounded output while waiting; otherwise the worker
            // can block on a full event channel before reaching the barrier.
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl TerminalApp {
    pub(super) fn open_conversations(&mut self) {
        if self.conversations.is_none() {
            self.toast_error(
                "Las conversaciones necesitan una instancia con permiso de guardar el perfil",
            );
            return;
        }
        let workspace = self.ws().id;
        let source = self.ws().focused_panel().map(|p| p.focused_leaf_id());
        if let Some(hub) = self.conversations.as_mut() {
            if hub.workspace != workspace {
                // A draft retains its original workspace until explicitly cleared.
                if !hub.draft.is_empty() {
                    hub.notice = Some("El borrador conserva su workspace de origen".into());
                } else {
                    hub.workspace = workspace;
                    hub.snapshot = HubSnapshot {
                        conversations: vec![],
                        connected: vec![],
                        links: vec![],
                        transfers: vec![],
                    };
                    hub.source = source;
                    hub.target = None;
                    hub.history.clear();
                    hub.selected.clear();
                    hub.preview = None;
                }
            }
            if hub.source.is_none() {
                hub.source = source;
            }
            hub.open = true;
            hub.job(Job::Snapshot(hub.workspace));
            if let Some(source) = hub.source {
                hub.job(Job::History(source, 0));
            }
        }
    }
    pub(super) fn start_conversations(&mut self, ctx: &egui::Context) {
        if !self.ensure_persistence_ownership() {
            return;
        }
        match ConversationUi::new(ctx.clone(), self.ws().id) {
            Ok(hub) => self.conversations = Some(hub),
            Err(error) => self.toast_error(format!(
                "No se pudo iniciar el historial de conversaciones: {error}"
            )),
        }
    }
    pub(super) fn poll_conversations(&mut self, ctx: &egui::Context) {
        let Some(mut hub) = self.conversations.take() else {
            return;
        };
        let enabled = self.ensure_persistence_ownership()
            && !matches!(self.collab.mode(), crate::collab::CollabMode::Guest);
        hub.worker.active(enabled);
        // Receipts cannot be silently lost when the command channel is full.
        while let Some(job) = hub.receipts.pop_front() {
            match hub.worker.jobs.try_send(job) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(job)) => {
                    hub.receipts.push_front(job);
                    break;
                }
                Err(_) => {
                    hub.error = Some(
                        "No se pudo guardar la confirmación; la entrega queda sin confirmar".into(),
                    );
                    break;
                }
            }
        }
        while let Ok(event) = hub.worker.events.try_recv() {
            if let Event::Attention(id) = event {
                self.toast_error(format!("Un envío entre chats necesita revisión ({id}). Abrí Conversaciones y contexto."));
                continue;
            }
            if let Some((transfer, target)) = hub.consume(event) {
                let result = if enabled {
                    self.deliver_conversation_transfer(&transfer, &target, &hub.bindings)
                } else {
                    Err("La ventana dejó de tener permiso de entrega".into())
                };
                hub.receipts.push_back(Job::Delivered(transfer.id, result));
            }
        }
        if enabled && hub.last_poll.elapsed() >= Duration::from_secs(1) {
            let mut rows = Vec::new();
            let mut seen = HashSet::new();
            for workspace in &self.workspaces {
                for panel in &workspace.panels {
                    for mut conversation in panel.conversation_leaves(workspace.id) {
                        seen.insert(conversation.id);
                        conversation.cwd = conversation.cwd.or_else(|| {
                            workspace
                                .cwd
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned())
                        });
                        if let Some(runtime) = conversation.runtime_session_id {
                            let binding = hub
                                .bindings
                                .entry(conversation.id)
                                .or_insert_with(|| (runtime, Uuid::new_v4()));
                            if binding.0 != runtime {
                                *binding = (runtime, Uuid::new_v4());
                            }
                            conversation.binding_id = Some(binding.1);
                        } else {
                            hub.bindings.remove(&conversation.id);
                        }
                        let idle = conversation.runtime_session_id.is_some_and(|runtime| {
                            self.orchestrator.sessions().iter().any(|s| {
                                s.runtime_session_id == Some(runtime)
                                    && matches!(
                                        s.status,
                                        crate::orchestration::AgentStatus::Idle
                                            | crate::orchestration::AgentStatus::Done
                                    )
                            })
                        });
                        rows.push(Binding {
                            conversation,
                            can_deliver: idle,
                        });
                    }
                }
            }
            hub.bindings.retain(|id, _| seen.contains(id));
            hub.job(Job::Observe(rows));
            if hub.open {
                hub.job(Job::Snapshot(hub.workspace));
            }
            hub.last_poll = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        self.conversations = Some(hub);
    }
    fn deliver_conversation_transfer(
        &self,
        transfer: &Transfer,
        target: &Conversation,
        bindings: &HashMap<Uuid, (Uuid, Uuid)>,
    ) -> Result<(), String> {
        let (runtime, binding) = bindings
            .get(&target.id)
            .copied()
            .ok_or("El destino no está conectado")?;
        if target.runtime_session_id != Some(runtime)
            || target.binding_id != Some(binding)
            || transfer.target_binding != Some(binding)
        {
            return Err("Cambió la sesión de destino".into());
        }
        let workspace = self
            .workspaces
            .iter()
            .find(|w| w.id == target.workspace_id)
            .ok_or("El workspace ya no existe")?;
        let panel = workspace
            .panel(target.panel_id)
            .ok_or("El panel ya no existe")?;
        let prompt = transfer.prompt().map_err(|e| e.to_string())?;
        panel
            .submit_prompt_to_leaf(target.id, runtime, &prompt)
            .map_err(|e| e.to_string())
    }
    pub(super) fn finish_conversations(&mut self) -> anyhow::Result<()> {
        let Some(hub) = self.conversations.as_mut() else {
            return Ok(());
        };
        hub.finish()
    }
    pub(super) fn show_conversations(&mut self, ctx: &egui::Context) {
        let Some(mut hub) = self.conversations.take() else {
            return;
        };
        if !hub.open {
            self.conversations = Some(hub);
            return;
        }
        let mut open = true;
        egui::Window::new("Conversaciones y contexto").id(egui::Id::new("conversation-hub"))
            .open(&mut open).default_size(egui::vec2(820.0,650.0)).resizable(true).show(ctx,|ui| {
                egui::ScrollArea::vertical().max_height(720.0).show(ui,|ui| {
                    ui.label("Historial propio de la app: mensajes registrados y selecciones guardadas. Los estados de entrega y de respuesta se muestran por separado.");
                    if let Some(error)=&hub.error {ui.colored_label(egui::Color32::LIGHT_RED,error);}
                    if let Some(notice)=&hub.notice {ui.label(notice);}
                    ui.horizontal(|ui| {
                        if ui.button("Actualizar").clicked(){hub.job(Job::Snapshot(hub.workspace));if let Some(id)=hub.source{hub.job(Job::History(id,0));}}
                        if ui.button("Limpiar aviso").clicked(){hub.error=None;hub.notice=None;}
                    });
                    ui.separator();
                    let previous=hub.source;
                    ui.horizontal(|ui| {
                        ui.label("Origen");conversation_picker(ui,"chat-source",&hub.snapshot.conversations,&mut hub.source,None);
                        ui.label("Destino");conversation_picker(ui,"chat-target",&hub.snapshot.conversations,&mut hub.target,hub.source);
                    });
                    if let Some(target)=hub.target {
                        ui.label(if hub.snapshot.connected.contains(&target){"Destino: MCP conectado. La cola espera a que el agente esté libre."}else{"Destino: MCP desconectado. Los mensajes permanecen en la cola sin iniciar procesos."});
                    }
                    if hub.source!=previous {hub.history.clear();hub.selected.clear();hub.preview=None;hub.detail=None;if let Some(id)=hub.source {hub.job(Job::History(id,0));}}
                    if let Some(source)=hub.source {
                        ui.horizontal(|ui| {
                            if ui.button("Configurar MCP de este chat").clicked(){hub.job(Job::Config(source));}
                            if ui.button("Revocar acceso MCP").clicked(){hub.job(Job::Revoke(source));}
                            let selection=self.workspaces.iter().find(|w|w.id==hub.workspace).and_then(|w|w.focused_panel()).filter(|p|p.focused_leaf_id()==source).and_then(|p|p.selected_text());
                            if ui.add_enabled(selection.is_some(),egui::Button::new("Guardar selección del terminal")).clicked(){if let Some(text)=selection{hub.job(Job::Record{id:Uuid::new_v4(),thread:source,text});}}
                        });
                    }
                    if let (Some(source),Some(target))=(hub.source,hub.target) {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut hub.automatic,"Permitir envíos automáticos entre estos chats");
                            if ui.button("Guardar vínculo en ambas direcciones").clicked(){hub.job(Job::Link(source,target,hub.automatic));}
                            if ui.button("Quitar vínculo de origen a destino").clicked(){hub.job(Job::Unlink(source,target));}
                        });
                        let link=hub.snapshot.links.iter().find(|l|l.source_id==source&&l.target_id==target);
                        ui.label(match link {Some(l) if l.automatic_delivery=>"Vínculo autorizado para enviar y consultar contexto",Some(_)=>"Vínculo con aprobación por mensaje",None=>"Sin vínculo: el usuario puede transferir contexto; los agentes no pueden iniciar envíos"});
                    }
                    ui.separator();ui.strong("Mensajes para incluir como contexto");
                    for entry in hub.history.clone() {
                        ui.horizontal(|ui| {
                            let mut selected=hub.selected.contains(&entry.id);
                            if ui.checkbox(&mut selected,format!("{:?} · {} bytes · {}",entry.role,entry.bytes,entry.preview)).changed(){if selected{hub.selected.insert(entry.id);}else{hub.selected.remove(&entry.id);}}
                            if ui.small_button("Leer completo").clicked(){hub.job(Job::Message(entry.id));}
                        });
                    }
                    if hub.history.is_empty(){ui.label("Todavía no hay mensajes registrados. Guardá una selección o usá tc_chat_record desde el agente.");}
                    if !hub.history_end && !hub.history.is_empty() && ui.button("Más mensajes").clicked(){if let(Some(source),Some(last))=(hub.source,hub.history.last()){hub.job(Job::History(source,last.sequence));}}
                    ui.horizontal(|ui| {ui.label("Presupuesto de contexto (bytes)");ui.add(egui::Slider::new(&mut hub.budget,1024..=MAX_CONTEXT_BYTES).logarithmic(true));});
                    ui.label("Instrucción nueva");ui.add(egui::TextEdit::multiline(&mut hub.draft).desired_rows(4).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(hub.pending.is_none()&&hub.request().is_some(),egui::Button::new("Preparar y revisar contexto")).clicked(){if let Some(request)=hub.request(){hub.job(Job::Preview(request));}}
                        if ui.add_enabled(hub.pending.is_none()&&hub.preview_matches(),egui::Button::new("Guardar transferencia y enviar")).clicked(){if let Some(prepared)=hub.preview.clone(){let id=prepared.request.operation_id;if hub.job(Job::Send(Box::new(prepared))){hub.pending=Some(id);}}}
                    });
                    if let Some(prepared)=&hub.preview {
                        let context=&prepared.context;
                        ui.label(format!("{} mensajes completos · {} omitidos con referencia · {} / {} bytes",context.messages.len(),context.omitted.len(),context.used_bytes,context.budget_bytes));
                        if !hub.preview_matches(){ui.colored_label(egui::Color32::YELLOW,"Cambió el borrador: prepará otra vez el contexto");}
                        ui.collapsing("Ver contexto exacto",|ui|{for message in &context.messages {ui.strong(format!("{:?} · {}",message.role,message.id));ui.label(&message.text);}for reference in &context.omitted {ui.label(format!("Omitido: {} · {} bytes; recuperable por tc_chat_read",reference.id,reference.bytes));}});
                    }
                    ui.separator();ui.strong("Transferencias y respuestas");
                    for transfer in hub.snapshot.transfers.clone() {
                        ui.group(|ui| {
                            ui.label(format!("{} → {} · {}",chat_label(&hub.snapshot.conversations,Some(transfer.source_id)),chat_label(&hub.snapshot.conversations,Some(transfer.target_id)),transfer.state.label()));
                            ui.label(&transfer.request_preview);if let Some(error)=&transfer.error{ui.colored_label(egui::Color32::LIGHT_RED,error);}
                            if let Some(result)=&transfer.result_preview{ui.label(result);}
                            ui.horizontal(|ui| {
                                if ui.small_button("Abrir operación y respuesta").clicked(){hub.job(Job::Transfer(transfer.id));}
                                if transfer.state==TransferState::Pending && ui.small_button("Entregar ahora al terminal").on_hover_text("Verificá que el agente pueda recibir una instrucción. No inicia procesos ni cambia el foco.").clicked(){hub.job(Job::DeliverNow(transfer.id));}
                                if matches!(transfer.state,TransferState::AwaitingApproval|TransferState::NeedsReview)&&ui.small_button("Aprobar entrega").clicked(){hub.job(Job::Approve(transfer.id));}
                                if matches!(transfer.state,TransferState::AwaitingApproval|TransferState::Pending|TransferState::NeedsReview|TransferState::Failed)&&ui.small_button("Cancelar").clicked(){hub.job(Job::Cancel(transfer.id));}
                            });
                        });
                    }
                });
            });
        hub.open = open;
        if let Some(message) = hub.detail.as_ref() {
            let mut visible = true;
            egui::Window::new("Mensaje completo")
                .id(egui::Id::new("conversation-message"))
                .open(&mut visible)
                .default_size(egui::vec2(640.0, 400.0))
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{:?} · {} · origen: {}",
                        message.role, message.id, message.source
                    ));
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        text_page(ui, &message.text, &mut hub.message_offset);
                    });
                    if ui.button("Copiar texto exacto").clicked() {
                        ctx.copy_text(message.text.clone());
                    }
                });
            if !visible {
                hub.detail = None;
            }
        }
        if let Some(operation) = hub.operation.take() {
            let mut visible = true;
            egui::Window::new("Operación y resultado").id(egui::Id::new("conversation-operation")).open(&mut visible).default_size(egui::vec2(680.0,430.0)).show(ctx,|ui| {
                ui.label(format!("{} · {}",operation.id,operation.state.label()));
                egui::ScrollArea::vertical().max_height(250.0).show(ui,|ui| {
                    ui.strong("Instrucción");text_page(ui,&operation.request,&mut hub.request_offset);
                    if let Some(result)=&operation.result{ui.strong("Respuesta");text_page(ui,result,&mut hub.result_offset);}
                });
                ui.collapsing("Contexto conservado y referencias omitidas",|ui| {
                    for message in &operation.context.messages {ui.strong(format!("{:?} · {} · {}",message.role,message.id,message.origin.provider.map_or("Sin proveedor",|p|p.label())));ui.label(&message.text);}
                    for reference in &operation.context.omitted {ui.horizontal(|ui|{ui.label(format!("{} · {} bytes",reference.id,reference.bytes));if ui.button("Leer omitido").clicked(){hub.job(Job::Message(reference.id));}});}
                });
                if matches!(operation.state,TransferState::AwaitingApproval|TransferState::NeedsReview) && ui.button("Aprobar entrega de esta operación").clicked(){hub.job(Job::Approve(operation.id));}
                if matches!(operation.state,TransferState::Dispatching|TransferState::InputQueued) {
                    ui.label("Resultado explícito (también puede registrarlo el destinatario con tc_chat_reply)");ui.add(egui::TextEdit::multiline(&mut hub.reply).desired_rows(4));
                    if ui.add_enabled(!hub.reply.trim().is_empty(),egui::Button::new("Guardar respuesta completada")).clicked(){hub.job(Job::Reply(operation.id,hub.reply.clone()));}
                }
                if operation.state==TransferState::Dispatching {
                    ui.label("Una interrupción puede dejar la entrega sin confirmar. Verificá el chat de destino antes de decidir; la app no repite esta entrada.");
                    ui.horizontal(|ui| {
                        if ui.button("Confirmar que se entregó").clicked(){hub.job(Job::Resolve(operation.id,true));}
                        if ui.button("Confirmar que no se entregó").clicked(){hub.job(Job::Resolve(operation.id,false));}
                    });
                }
                if ui.button("Recargar operación").clicked(){hub.job(Job::Transfer(operation.id));}
            });
            if visible {
                hub.operation = Some(operation);
            }
        }
        self.conversations = Some(hub);
    }
}
fn text_page(ui: &mut egui::Ui, text: &str, offset: &mut usize) {
    *offset = (*offset).min(text.len());
    while !text.is_char_boundary(*offset) {
        *offset -= 1
    }
    let mut end = offset.saturating_add(32 * 1024).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1
    }
    ui.label(&text[*offset..end]);
    ui.horizontal(|ui| {
        ui.label(format!("Bytes {}–{} de {}", offset, end, text.len()));
        if ui
            .add_enabled(*offset > 0, egui::Button::new("Anterior"))
            .clicked()
        {
            *offset = offset.saturating_sub(32 * 1024);
        }
        if ui
            .add_enabled(end < text.len(), egui::Button::new("Siguiente"))
            .clicked()
        {
            *offset = end;
        }
    });
}
fn chat_label(rows: &[Conversation], id: Option<Uuid>) -> String {
    id.and_then(|id| rows.iter().find(|c| c.id == id))
        .map(|c| {
            format!(
                "{} · {} · {}",
                c.title,
                c.provider.map_or("Terminal", |p| p.label()),
                &c.id.to_string()[..8]
            )
        })
        .unwrap_or_else(|| {
            id.map(|id| id.to_string())
                .unwrap_or_else(|| "Elegir chat".into())
        })
}
fn conversation_picker(
    ui: &mut egui::Ui,
    key: &str,
    rows: &[Conversation],
    selected: &mut Option<Uuid>,
    exclude: Option<Uuid>,
) {
    egui::ComboBox::from_id_salt(key)
        .selected_text(chat_label(rows, *selected))
        .show_ui(ui, |ui| {
            for row in rows.iter().filter(|c| {
                Some(c.id) != exclude
                    && (key != "chat-target"
                        || c.provider
                            .is_some_and(|p| p != crate::orchestration::AgentProvider::Unknown))
            }) {
                ui.selectable_value(selected, Some(row.id), chat_label(rows, Some(row.id)));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{panel::TerminalPanel, pty::PtyHandle};
    use std::sync::{Arc, Mutex};

    struct Fixture {
        directory: std::path::PathBuf,
        app: TerminalApp,
        store: ConversationStore,
        target: Conversation,
        source: Conversation,
        handle: Arc<Mutex<PtyHandle>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.app.conversations.take();
            let old = std::mem::replace(&mut self.store, ConversationStore::in_memory().unwrap());
            drop(old);
            for _ in 0..100 {
                if std::fs::remove_dir_all(&self.directory).is_ok() || !self.directory.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    fn fixture() -> Fixture {
        fixture_with_capacity(64)
    }
    fn fixture_with_capacity(capacity: usize) -> Fixture {
        let ctx = egui::Context::default();
        let mut app = crate::app::preferences_ui_tests::detached_app();
        app.persistence_writes_enabled = true;
        let manager = app.workspaces[0].pty_manager();
        manager
            .lock()
            .unwrap()
            .set_remote_spawner(Box::new(|_, _, _, _, wanted| {
                Ok((
                    wanted.unwrap_or_else(Uuid::new_v4),
                    PtyHandle::for_persistence_tests(b""),
                ))
            }));
        let mut descriptors = Vec::new();
        let mut target_handle = None;
        for command in ["claude", "codex"] {
            let mut panel = TerminalPanel::new(
                egui::Pos2::ZERO,
                egui::vec2(500.0, 300.0),
                egui::Color32::WHITE,
                0,
            );
            panel.rename_title(command.to_owned());
            panel.attach_session_with_spec(
                Arc::clone(&manager),
                None,
                crate::runtime::SessionSpec::default(),
            );
            panel.set_agent_command(Some(command.into()));
            let mut row = panel.conversation_leaves(app.workspaces[0].id).remove(0);
            row.binding_id = Some(Uuid::new_v4());
            if command == "codex" {
                target_handle = panel.leaf_session_handle(row.id);
                panel.split_focused(
                    crate::terminal::split_tree::Axis::Horizontal,
                    Arc::clone(&manager),
                    None,
                    Some(app.workspaces[0].id),
                    80,
                    24,
                );
                assert_ne!(panel.focused_leaf_id(), row.id);
            }
            descriptors.push(row);
            app.workspaces[0].add_restored_terminal(panel);
        }
        app.workspaces[0].bring_to_front(descriptors[0].panel_id);
        let directory = std::env::temp_dir().join(format!("tc-chat-ui-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("history.db");
        let store = ConversationStore::open(&path).unwrap();
        for row in &descriptors {
            store.register(row).unwrap();
        }
        let token = store.issue_credential(descriptors[1].id).unwrap();
        store
            .renew_bridge(Uuid::new_v4(), descriptors[1].id, &token)
            .unwrap();
        store
            .set_available(Caller::Agent(descriptors[1].id), true)
            .unwrap();
        let source = descriptors[0].clone();
        let target = descriptors[1].clone();
        let worker = Worker::start_for_test(path, ctx, capacity).unwrap();
        worker
            .submit(Job::Observe(
                descriptors
                    .iter()
                    .map(|row| Binding {
                        conversation: row.clone(),
                        can_deliver: false,
                    })
                    .collect(),
            ))
            .unwrap();
        let mut hub = ConversationUi::with_worker(worker, app.workspaces[0].id);
        hub.open = true;
        hub.source = Some(source.id);
        hub.target = Some(target.id);
        hub.draft = "  Revisá la respuesta con Codex\n日本語  ".into();
        hub.snapshot = store.snapshot(Caller::User, source.workspace_id).unwrap();
        hub.last_poll = Instant::now();
        for row in descriptors {
            hub.bindings.insert(
                row.id,
                (row.runtime_session_id.unwrap(), row.binding_id.unwrap()),
            );
        }
        app.conversations = Some(hub);
        Fixture {
            directory,
            app,
            store,
            target,
            source,
            handle: target_handle.unwrap(),
        }
    }
    fn frame(
        app: &mut TerminalApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1000.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.show_conversations(ui.ctx()),
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    text.pos + text.galley.rect.center().to_vec2(),
                )),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        output.drop_without_applying_deltas();
        labels
    }
    fn click(app: &mut TerminalApp, ctx: &egui::Context, label: &str) {
        frame(app, ctx, vec![]);
        let labels = frame(app, ctx, vec![]);
        let pos = labels
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| {
                panic!(
                    "Missing {label}: {:?}",
                    labels.iter().map(|(t, _)| t).collect::<Vec<_>>()
                )
            })
            .1;
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    fn pump_until(
        fixture: &mut Fixture,
        ctx: &egui::Context,
        condition: impl Fn(&Fixture) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition(fixture) {
            assert!(Instant::now() < deadline, "Worker did not finish");
            fixture.app.poll_conversations(ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn hub_ui_previews_persists_delivers_to_unfocused_split_and_collects_peer_reply() {
        let mut fixture = fixture();
        let ctx = egui::Context::default();
        let message = fixture
            .store
            .record(
                Caller::User,
                Uuid::new_v4(),
                fixture.source.id,
                MessageRole::Assistant,
                "Contexto completo 👩🏽‍💻",
                "test",
            )
            .unwrap();
        let hub = fixture.app.conversations.as_mut().unwrap();
        hub.history = fixture
            .store
            .history(Caller::User, fixture.source.id, 0)
            .unwrap();
        hub.selected.insert(message.id);
        hub.history_end = true;
        click(&mut fixture.app, &ctx, "Preparar y revisar contexto");
        pump_until(&mut fixture, &ctx, |f| {
            f.app.conversations.as_ref().unwrap().preview.is_some()
        });
        let prepared = fixture
            .app
            .conversations
            .as_ref()
            .unwrap()
            .preview
            .clone()
            .unwrap();
        assert_eq!(prepared.context.messages[0].text, message.text);
        click(&mut fixture.app, &ctx, "Guardar transferencia y enviar");
        pump_until(&mut fixture, &ctx, |f| {
            f.store
                .transfer(Caller::User, prepared.request.operation_id)
                .is_ok_and(|op| op.state == TransferState::InputQueued)
        });
        assert_eq!(fixture.handle.lock().unwrap().activity_revision(), 1);
        assert_eq!(
            fixture.app.ws().focused_panel().unwrap().id(),
            fixture.source.panel_id
        );
        assert_ne!(
            fixture
                .app
                .ws()
                .panel(fixture.target.panel_id)
                .unwrap()
                .focused_leaf_id(),
            fixture.target.id
        );
        fixture
            .store
            .reply(
                Caller::Agent(fixture.target.id),
                prepared.request.operation_id,
                "Respuesta explícita de Codex",
            )
            .unwrap();
        fixture
            .app
            .conversations
            .as_mut()
            .unwrap()
            .job(Job::Snapshot(fixture.source.workspace_id));
        pump_until(&mut fixture, &ctx, |f| {
            f.app
                .conversations
                .as_ref()
                .unwrap()
                .snapshot
                .transfers
                .iter()
                .any(|t| {
                    t.id == prepared.request.operation_id && t.state == TransferState::Completed
                })
        });
        fixture.app.finish_conversations().unwrap();
        assert_eq!(fixture.handle.lock().unwrap().activity_revision(), 1);
    }
    #[test]
    fn saved_ack_preserves_a_draft_edited_while_save_was_pending() {
        let mut fixture = fixture();
        let hub = fixture.app.conversations.as_mut().unwrap();
        let request = hub.request().unwrap();
        hub.preview = Some(PreparedSend {
            context: fixture
                .store
                .context(Caller::User, fixture.source.id, &[], DEFAULT_CONTEXT_BYTES)
                .unwrap(),
            target_binding: fixture.target.binding_id,
            request: request.clone(),
        });
        hub.pending = Some(request.operation_id);
        hub.draft = "Borrador nuevo mientras se guarda el anterior".into();
        hub.consume(Event::Saved(request.operation_id));
        assert_eq!(hub.draft, "Borrador nuevo mientras se guarda el anterior");
        assert!(hub.pending.is_none());
    }
    #[test]
    fn changed_runtime_or_binding_does_not_write_to_any_leaf() {
        let fixture = fixture();
        let request = fixture
            .app
            .conversations
            .as_ref()
            .unwrap()
            .request()
            .unwrap();
        let mut store = ConversationStore::open(&fixture.directory.join("history.db")).unwrap();
        let transfer = store.send(Caller::User, &request).unwrap();
        let mut target = fixture.target.clone();
        target.runtime_session_id = Some(Uuid::new_v4());
        assert!(fixture
            .app
            .deliver_conversation_transfer(
                &transfer,
                &target,
                &fixture.app.conversations.as_ref().unwrap().bindings
            )
            .is_err());
        assert_eq!(fixture.handle.lock().unwrap().activity_revision(), 0);
    }
    #[test]
    fn shutdown_drains_full_channels_and_persists_late_delivery_without_terminal_input() {
        // Both queues must actually be full, but disk speed must not determine
        // whether the test can reach that state before shutdown starts.
        const CAPACITY: usize = 2;
        let mut fixture = fixture_with_capacity(CAPACITY);
        let hub = fixture.app.conversations.as_mut().unwrap();
        hub.worker.active(false);
        let mut ids = Vec::new();
        for _ in 0..CAPACITY {
            let id = Uuid::new_v4();
            hub.worker
                .jobs
                .send(Job::Record {
                    id,
                    thread: fixture.source.id,
                    text: "Registro aceptado antes del cierre".into(),
                })
                .unwrap();
            ids.push(id);
        }
        // The worker has filled its output channel and must block on the next
        // event. Queue a delivery and fill its input channel as well.
        let deadline = Instant::now() + Duration::from_secs(30);
        while fixture
            .store
            .message(Caller::User, *ids.last().unwrap())
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let request = hub.request().unwrap();
        let mut store = ConversationStore::open(&fixture.directory.join("history.db")).unwrap();
        store.send(Caller::User, &request).unwrap();
        hub.worker
            .jobs
            .send(Job::DeliverNow(request.operation_id))
            .unwrap();
        for _ in 0..CAPACITY {
            let id = Uuid::new_v4();
            hub.worker
                .jobs
                .send(Job::Record {
                    id,
                    thread: fixture.source.id,
                    text: "Registro en la cola llena".into(),
                })
                .unwrap();
            ids.push(id);
        }
        fixture.app.finish_conversations().unwrap();
        for id in ids {
            assert!(store.message(Caller::User, id).unwrap().is_some());
        }
        let transfer = store.transfer(Caller::User, request.operation_id).unwrap();
        assert_eq!(transfer.state, TransferState::Failed);
        assert!(transfer.error.unwrap().contains("Cierre de la app"));
        assert_eq!(fixture.handle.lock().unwrap().activity_revision(), 0);
    }
}
