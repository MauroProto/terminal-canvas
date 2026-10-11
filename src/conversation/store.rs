use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::*;

pub struct ConversationStore {
    db: Connection,
}

impl ConversationStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path).context("abrir el historial de conversaciones")?;
        Self::initialize(db)
    }
    pub fn in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }
    fn initialize(db: Connection) -> Result<Self> {
        db.busy_timeout(Duration::from_secs(2))?;
        let initial_version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            initial_version <= 1,
            "El historial pertenece a una versión más nueva de Terminal Canvas"
        );
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let tx = rusqlite::Transaction::new_unchecked(&db, TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            version <= 1,
            "El historial pertenece a una versión más nueva de Terminal Canvas"
        );
        tx.execute_batch("
            CREATE TABLE IF NOT EXISTS conversations(id TEXT PRIMARY KEY, workspace TEXT NOT NULL, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS messages(sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
                conversation TEXT NOT NULL REFERENCES conversations(id), role TEXT NOT NULL, text TEXT NOT NULL,
                source TEXT NOT NULL, created INTEGER NOT NULL, origin TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS messages_thread ON messages(conversation, sequence);
            CREATE TABLE IF NOT EXISTS links(source TEXT NOT NULL REFERENCES conversations(id),
                target TEXT NOT NULL REFERENCES conversations(id), automatic INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(source,target));
            CREATE TABLE IF NOT EXISTS credentials(conversation TEXT PRIMARY KEY REFERENCES conversations(id),
                digest BLOB NOT NULL, enabled INTEGER NOT NULL DEFAULT 1);
            CREATE TABLE IF NOT EXISTS bridges(id TEXT PRIMARY KEY,conversation TEXT NOT NULL REFERENCES conversations(id),digest BLOB NOT NULL,expires INTEGER NOT NULL,available INTEGER);
            CREATE TABLE IF NOT EXISTS transfers(id TEXT PRIMARY KEY, source TEXT NOT NULL REFERENCES conversations(id),
                target TEXT NOT NULL REFERENCES conversations(id), state TEXT NOT NULL, fingerprint TEXT NOT NULL,
                data TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS transfer_state ON transfers(state);
            CREATE INDEX IF NOT EXISTS transfer_root ON transfers(json_extract(data,'$.root_id'));
            CREATE INDEX IF NOT EXISTS transfer_parent ON transfers(json_extract(data,'$.parent_id'));
            PRAGMA user_version=1;")?;
        tx.commit()?;
        Ok(Self { db })
    }

    pub fn register(&self, conversation: &Conversation) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut conversation = conversation.clone();
        anyhow::ensure!(
            conversation.title.len() <= 4096
                && conversation.cwd.as_ref().map_or(0, String::len) <= 32768
                && conversation
                    .native_session_id
                    .as_ref()
                    .map_or(0, String::len)
                    <= 256,
            "Metadatos de conversación demasiado grandes"
        );
        if let Some(previous) = self.conversation(conversation.id)? {
            anyhow::ensure!(
                previous.workspace_id == conversation.workspace_id
                    && previous.panel_id == conversation.panel_id,
                "La identidad de conversación ya pertenece a otro workspace o panel"
            );
            conversation.provider = conversation.provider.or(previous.provider);
            if previous == conversation {
                tx.commit()?;
                return Ok(());
            }
        }
        self.db.execute(
            "INSERT INTO conversations(id,workspace,data) VALUES(?1,?2,?3)
            ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![
                conversation.id.to_string(),
                conversation.workspace_id.to_string(),
                serde_json::to_string(&conversation)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn conversation(&self, id: Uuid) -> Result<Option<Conversation>> {
        let data: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM conversations WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| serde_json::from_str(&data).map_err(Into::into))
            .transpose()
    }

    pub fn authorize(&self, caller: Caller, target: Uuid) -> Result<Conversation> {
        let conversation = self
            .conversation(target)?
            .context("La conversación no existe")?;
        if let Some(source) = caller.agent_id() {
            let digest: Option<Vec<u8>> = self
                .db
                .query_row(
                    "SELECT digest FROM credentials WHERE conversation=?1 AND enabled=1",
                    [source.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            anyhow::ensure!(digest.is_some(), "Credencial de chat revocada");
            if let Caller::AuthenticatedAgent(auth) = caller {
                anyhow::ensure!(
                    digest
                        .as_ref()
                        .is_some_and(|current| credential_matches(current, &auth.digest)),
                    "Credencial de chat renovada; reconectá el puente MCP"
                );
            }
            let own = self
                .conversation(source)?
                .context("El chat llamante no existe")?;
            let linked: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM links WHERE source=?1 AND target=?2)",
                params![source.to_string(), target.to_string()],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                own.workspace_id == conversation.workspace_id && (source == target || linked),
                "El chat no está vinculado al alcance autorizado"
            );
        }
        Ok(conversation)
    }

    pub fn link(&self, source: Uuid, target: Uuid, automatic: bool) -> Result<()> {
        anyhow::ensure!(source != target, "Elegí dos conversaciones distintas");
        let a = self.authorize(Caller::User, source)?;
        let b = self.authorize(Caller::User, target)?;
        anyhow::ensure!(
            a.workspace_id == b.workspace_id,
            "No se vinculan chats de otros workspaces"
        );
        self.db.execute(
            "INSERT INTO links(source,target,automatic) VALUES(?1,?2,?3)
            ON CONFLICT(source,target) DO UPDATE SET automatic=excluded.automatic",
            params![source.to_string(), target.to_string(), automatic],
        )?;
        Ok(())
    }
    pub fn link_pair(&self, source: Uuid, target: Uuid, automatic: bool) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        self.link(source, target, automatic)?;
        self.link(target, source, automatic)?;
        tx.commit()?;
        Ok(())
    }
    pub fn unlink(&self, source: Uuid, target: Uuid) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        self.db.execute(
            "DELETE FROM links WHERE source=?1 AND target=?2",
            params![source.to_string(), target.to_string()],
        )?;
        // Queued agent requests lose permission immediately; dispatched input cannot be recalled.
        self.db.execute("UPDATE transfers SET state='cancelled', data=json_set(data,'$.state','cancelled',
            '$.error','Vínculo revocado antes de la entrega') WHERE source=?1 AND target=?2 AND json_extract(data,'$.requested_by_agent')=1 AND state IN ('pending','awaiting_approval')",
            params![source.to_string(), target.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn issue_credential(&self, id: Uuid) -> Result<String> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.activate_credential(id, &token)?;
        Ok(token)
    }
    pub(crate) fn activate_credential(&self, id: Uuid, token: &str) -> Result<()> {
        self.authorize(Caller::User, id)?;
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        self.db.execute(
            "INSERT INTO credentials(conversation,digest,enabled) VALUES(?1,?2,1)
            ON CONFLICT(conversation) DO UPDATE SET digest=excluded.digest,enabled=1",
            params![id.to_string(), digest],
        )?;
        Ok(())
    }
    pub fn authenticate(&self, id: Uuid, token: &str) -> Result<Caller> {
        let digest: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT digest FROM credentials WHERE conversation=?1 AND enabled=1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let actual = Sha256::digest(token.as_bytes());
        let valid = digest
            .as_ref()
            .is_some_and(|expected| credential_matches(expected, &actual));
        anyhow::ensure!(valid, "Credencial de chat inválida o revocada");
        Ok(Caller::AuthenticatedAgent(AgentAuthentication {
            id,
            digest: actual.into(),
        }))
    }
    pub fn revoke(&self, id: Uuid) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        self.db.execute(
            "UPDATE credentials SET enabled=0 WHERE conversation=?1",
            [id.to_string()],
        )?;
        self.db.execute("UPDATE transfers SET state='cancelled',data=json_set(data,'$.state','cancelled','$.error','Credencial revocada antes de entregar') WHERE source=?1 AND json_extract(data,'$.requested_by_agent')=1 AND state IN ('pending','awaiting_approval','needs_review')",[id.to_string()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn renew_bridge(&self, lease: Uuid, id: Uuid, token: &str) -> Result<()> {
        self.authenticate(id, token)?;
        self.db.execute("INSERT INTO bridges(id,conversation,digest,expires) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET expires=excluded.expires",params![lease.to_string(),id.to_string(),Sha256::digest(token.as_bytes()).to_vec(),chrono::Utc::now().timestamp_millis()+15_000])?;
        self.db.execute(
            "DELETE FROM bridges WHERE expires<?1",
            [chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }
    pub fn remove_bridge(&self, lease: Uuid) -> Result<()> {
        self.db
            .execute("DELETE FROM bridges WHERE id=?1", [lease.to_string()])?;
        Ok(())
    }
    pub fn bridge_connected(&self, id: Uuid) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM bridges JOIN credentials USING(conversation) WHERE conversation=?1 AND bridges.digest=credentials.digest AND credentials.enabled=1 AND expires>?2)",params![id.to_string(),chrono::Utc::now().timestamp_millis()],|row|row.get(0))?)
    }
    pub fn bridge_availability(&self, id: Uuid) -> Result<Option<bool>> {
        Ok(self.db.query_row("SELECT MIN(available) FROM bridges JOIN credentials USING(conversation) WHERE conversation=?1 AND bridges.digest=credentials.digest AND credentials.enabled=1 AND expires>?2",params![id.to_string(),chrono::Utc::now().timestamp_millis()],|row|row.get(0))?)
    }
    pub fn set_available(&self, caller: Caller, available: bool) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let Some(id) = caller.agent_id() else {
            anyhow::bail!("La disponibilidad la declara el chat receptor")
        };
        self.authorize(caller, id)?;
        self.db.execute("UPDATE bridges SET available=?1 WHERE conversation=?2 AND digest=(SELECT digest FROM credentials WHERE conversation=?2 AND enabled=1)",params![available,id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn record(
        &self,
        caller: Caller,
        id: Uuid,
        thread: Uuid,
        role: MessageRole,
        text: &str,
        source: &str,
    ) -> Result<ConversationMessage> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some(own) = caller.agent_id() {
            anyhow::ensure!(
                own == thread,
                "Sólo se registran mensajes en el chat propio"
            );
        }
        let conversation = self.authorize(caller, thread)?;
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= MAX_MESSAGE_BYTES && source.len() <= 256,
            "Mensaje vacío o demasiado grande"
        );
        if let Some(old) = self.message(caller, id)? {
            anyhow::ensure!(
                old.conversation_id == thread
                    && old.role == role
                    && old.text == text
                    && old.source == source,
                "El identificador ya representa otro mensaje"
            );
            tx.commit()?;
            return Ok(old);
        }
        self.db.execute("INSERT INTO messages(id,conversation,role,text,source,created,origin) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id.to_string(),thread.to_string(),serde_json::to_string(&role)?,text,source,chrono::Utc::now().timestamp_millis(),serde_json::to_string(&MessageOrigin::from(&conversation))?])?;
        let message = self
            .message(caller, id)?
            .context("No se pudo leer el mensaje guardado")?;
        tx.commit()?;
        Ok(message)
    }

    pub fn message(&self, caller: Caller, id: Uuid) -> Result<Option<ConversationMessage>> {
        let value = self.db.query_row("SELECT id,sequence,conversation,role,text,source,created,origin FROM messages WHERE id=?1", [id.to_string()], read_message).optional()?;
        if let Some(message) = &value {
            if let Err(error) = self.authorize(caller, message.conversation_id) {
                let Some(own) = caller.agent_id() else {
                    return Err(error);
                };
                self.authorize(caller, own)?;
                let referenced:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM transfers, json_each(json_extract(data,'$.context.messages')) AS item WHERE target=?1 AND source=?2 AND state IN ('dispatching','input_queued','completed') AND json_extract(item.value,'$.id')=?3 UNION ALL SELECT 1 FROM transfers, json_each(json_extract(data,'$.context.omitted')) AS item WHERE target=?1 AND source=?2 AND state IN ('dispatching','input_queued','completed') AND json_extract(item.value,'$.id')=?3)",params![own.to_string(),message.conversation_id.to_string(),id.to_string()],|row|row.get(0))?;
                anyhow::ensure!(referenced, "El mensaje no está en el contexto autorizado");
            }
        }
        Ok(value)
    }
    pub fn messages(
        &self,
        caller: Caller,
        thread: Uuid,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ConversationMessage>> {
        self.authorize(caller, thread)?;
        let mut query = self.db.prepare(
            "SELECT id,sequence,conversation,role,text,source,created,origin FROM messages
            WHERE conversation=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let result = query
            .query_map(
                params![thread.to_string(), after, limit.clamp(1, 100)],
                read_message,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(result)
    }

    pub fn context(
        &self,
        caller: Caller,
        thread: Uuid,
        ids: &[Uuid],
        budget: usize,
    ) -> Result<ContextSnapshot> {
        anyhow::ensure!(
            (1024..=MAX_CONTEXT_BYTES).contains(&budget),
            "Presupuesto de contexto fuera de 1–64 KiB"
        );
        anyhow::ensure!(
            ids.len() <= 100,
            "Elegí como máximo 100 mensajes por transferencia"
        );
        let source = self.authorize(caller, thread)?;
        let mut context = ContextSnapshot {
            protocol_version: 1,
            source,
            messages: Vec::new(),
            omitted: Vec::new(),
            budget_bytes: budget,
            used_bytes: 0,
        };
        for id in ids {
            anyhow::ensure!(
                !context.omitted.iter().any(|m| m.id == *id),
                "Mensaje de contexto repetido"
            );
            let reference=self.db.query_row("SELECT id,sequence,role,length(CAST(text AS BLOB)) FROM messages WHERE id=?1 AND conversation=?2",params![id.to_string(),thread.to_string()],|row| {
                let role:String=row.get(2)?;let role=serde_json::from_str(&role).map_err(|error|rusqlite::Error::FromSqlConversionFailure(2,rusqlite::types::Type::Text,Box::new(error)))?;
                Ok(MessageReference{id:uuid_column(row,0)?,sequence:row.get(1)?,role,bytes:row.get(3)?})
            }).optional()?.context("El mensaje seleccionado no existe en el chat de origen")?;
            context.omitted.push(reference);
        }
        measure_context(&mut context)?;
        anyhow::ensure!(
            context.used_bytes <= budget,
            "Ni las referencias al historial omitido caben en el presupuesto"
        );
        for id in ids {
            let message = self
                .message(caller, *id)?
                .context("El mensaje seleccionado ya no existe")?;
            anyhow::ensure!(
                message.conversation_id == thread,
                "El mensaje no pertenece al chat de origen"
            );
            let position = context
                .omitted
                .iter()
                .position(|reference| reference.id == *id)
                .context("Falta la referencia de contexto")?;
            let reference = context.omitted.remove(position);
            context.messages.push(message);
            measure_context(&mut context)?;
            if context.used_bytes > budget {
                context.messages.pop();
                context.omitted.insert(position, reference);
            }
        }
        context.messages.sort_by_key(|message| message.sequence);
        context.omitted.sort_by_key(|message| message.sequence);
        measure_context(&mut context)?;
        anyhow::ensure!(
            context.used_bytes <= budget,
            "Ni las referencias al historial omitido caben en el presupuesto"
        );
        Ok(context)
    }

    pub fn send(&mut self, caller: Caller, request: &SendRequest) -> Result<Transfer> {
        self.send_inner(caller, request, None)
    }
    pub fn send_prepared(&mut self, caller: Caller, prepared: &PreparedSend) -> Result<Transfer> {
        self.send_inner(caller, &prepared.request, Some(prepared))
    }
    fn send_inner(
        &mut self,
        caller: Caller,
        request: &SendRequest,
        prepared: Option<&PreparedSend>,
    ) -> Result<Transfer> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        anyhow::ensure!(
            !request.text.trim().is_empty() && request.text.len() <= MAX_MESSAGE_BYTES,
            "Instrucción vacía o demasiado grande"
        );
        anyhow::ensure!(
            request.source_id != request.target_id,
            "Elegí otro chat de destino"
        );
        if let Some(own) = caller.agent_id() {
            anyhow::ensure!(
                own == request.source_id,
                "No se puede suplantar al chat de origen"
            );
        }
        let source = self.authorize(caller, request.source_id)?;
        let target = self.authorize(caller, request.target_id)?;
        anyhow::ensure!(
            source.workspace_id == target.workspace_id,
            "Origen y destino pertenecen a distintos workspaces"
        );
        let fingerprint = serde_json::to_string(&(request, prepared))?;
        let existing: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT fingerprint,data FROM transfers WHERE id=?1",
                [request.operation_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((old, data)) = existing {
            anyhow::ensure!(
                old == fingerprint,
                "El identificador de operación ya tiene otro contenido"
            );
            let transfer: Transfer = serde_json::from_str(&data)?;
            self.check_transfer_access(caller, &transfer)?;
            tx.commit()?;
            return Ok(transfer);
        }
        let context = self.context(
            caller,
            request.source_id,
            &request.message_ids,
            request.context_budget_bytes,
        )?;
        if let Some(prepared) = prepared {
            anyhow::ensure!(prepared.context==context && prepared.target_binding==target.binding_id,"Cambió el contexto o la sesión de destino; prepará y revisá otra vez la transferencia");
        }
        let automatic: bool = match caller {
            Caller::User => true,
            Caller::Agent(_) | Caller::AuthenticatedAgent(_) => self.db.query_row(
                "SELECT automatic FROM links WHERE source=?1 AND target=?2",
                params![request.source_id.to_string(), request.target_id.to_string()],
                |row| row.get(0),
            )?,
        };
        let parent_id: Option<Uuid> = if let Some(own) = caller.agent_id() {
            self.db.query_row("SELECT id FROM transfers WHERE target=?1 AND state IN ('dispatching','input_queued') ORDER BY rowid LIMIT 1",[own.to_string()],|row|uuid_column(row,0)).optional()?
        } else {
            None
        };
        let mut ancestor = parent_id;
        let mut depth = 1;
        let mut root_id = request.operation_id;
        while let Some(id) = ancestor {
            let parent = self.transfer(Caller::User, id)?;
            anyhow::ensure!(parent.source_id!=request.target_id && parent.target_id!=request.target_id,"La delegación volvería a un chat de su propia cadena; respondé la operación existente");
            root_id = parent.root_id;
            depth += 1;
            anyhow::ensure!(
                depth <= MAX_DELEGATION_DEPTH,
                "La delegación supera el límite de 8 niveles"
            );
            ancestor = parent.parent_id;
        }
        let total: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM transfers WHERE json_extract(data,'$.root_id')=?1",
            [root_id.to_string()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            total < MAX_ROOT_TRANSFERS as i64,
            "La tarea alcanzó el límite de 64 transferencias"
        );
        let transfer = Transfer {
            id: request.operation_id,
            source_id: request.source_id,
            target_id: request.target_id,
            parent_id,
            root_id,
            depth,
            request: request.text.clone(),
            context,
            state: if automatic {
                TransferState::Pending
            } else {
                TransferState::AwaitingApproval
            },
            target_binding: target.binding_id,
            requested_by_agent: caller.agent_id().is_some(),
            result: None,
            error: None,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        transfer.prompt()?;
        let pending:i64=tx.query_row("SELECT COUNT(*) FROM transfers WHERE target=?1 AND state IN ('pending','awaiting_approval','dispatching','input_queued','needs_review')",
            [target.id.to_string()],|row|row.get(0))?;
        anyhow::ensure!(
            pending < MAX_PENDING_TRANSFERS as i64,
            "La cola del chat está llena; esperá o cancelá mensajes pendientes"
        );
        tx.execute("INSERT INTO transfers(id,source,target,state,fingerprint,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![transfer.id.to_string(),transfer.source_id.to_string(),transfer.target_id.to_string(),state_name(transfer.state),fingerprint,serde_json::to_string(&transfer)?])?;
        tx.execute("INSERT INTO messages(id,conversation,role,text,source,created,origin) VALUES(?1,?2,?3,?4,'outgoing_request',?5,?6)",
            params![Uuid::new_v4().to_string(),transfer.source_id.to_string(),serde_json::to_string(&if transfer.requested_by_agent{MessageRole::Activity}else{MessageRole::User})?,transfer.request,transfer.created_at,serde_json::to_string(&MessageOrigin::from(&source))?])?;
        tx.commit()?;
        Ok(transfer)
    }

    fn check_transfer_access(&self, caller: Caller, transfer: &Transfer) -> Result<()> {
        if let Some(own) = caller.agent_id() {
            self.authorize(caller, own)?;
            anyhow::ensure!(
                own == transfer.source_id || own == transfer.target_id,
                "La operación pertenece a otros chats"
            );
        }
        Ok(())
    }
    pub fn transfer(&self, caller: Caller, id: Uuid) -> Result<Transfer> {
        let data: String = self.db.query_row(
            "SELECT data FROM transfers WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        let transfer = serde_json::from_str(&data)?;
        self.check_transfer_access(caller, &transfer)?;
        Ok(transfer)
    }
    pub fn approve(&self, id: Uuid) -> Result<()> {
        let mut transfer = self.transfer(Caller::User, id)?;
        anyhow::ensure!(
            matches!(
                transfer.state,
                TransferState::AwaitingApproval | TransferState::NeedsReview
            ),
            "La operación ya no espera aprobación"
        );
        transfer.target_binding = self.authorize(Caller::User, transfer.target_id)?.binding_id;
        anyhow::ensure!(
            transfer.target_binding.is_some(),
            "El chat de destino no está conectado"
        );
        self.transition(&mut transfer, TransferState::Pending, None)
    }
    pub fn cancel(&self, caller: Caller, id: Uuid) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut transfer = self.transfer(caller, id)?;
        anyhow::ensure!(
            matches!(
                transfer.state,
                TransferState::AwaitingApproval
                    | TransferState::Pending
                    | TransferState::NeedsReview
                    | TransferState::Failed
            ),
            "La entrada ya pudo haber sido enviada; no puede retirarse"
        );
        self.transition(&mut transfer, TransferState::Cancelled, None)?;
        tx.commit()?;
        Ok(())
    }
    fn transition(
        &self,
        transfer: &mut Transfer,
        next: TransferState,
        error: Option<String>,
    ) -> Result<()> {
        let previous = state_name(transfer.state);
        transfer.state = next;
        transfer.error = error;
        let changed = self.db.execute(
            "UPDATE transfers SET state=?1,data=?2 WHERE id=?3 AND state=?4",
            params![
                state_name(next),
                serde_json::to_string(transfer)?,
                transfer.id.to_string(),
                previous
            ],
        )?;
        anyhow::ensure!(
            changed == 1,
            "La operación cambió en otro proceso; recargá su estado"
        );
        Ok(())
    }
    pub fn claim(&self, id: Uuid, binding: Uuid) -> Result<Option<Transfer>> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut transfer = self.transfer(Caller::User, id)?;
        if transfer.state != TransferState::Pending {
            return Ok(None);
        }
        if !self.bridge_connected(transfer.target_id)? {
            return Ok(None);
        }
        let busy:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM transfers WHERE target=?1 AND id<>?2 AND state IN ('dispatching','input_queued'))",params![transfer.target_id.to_string(),id.to_string()],|row|row.get(0))?;
        if busy {
            return Ok(None);
        }
        if transfer.requested_by_agent {
            let linked:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM links JOIN credentials ON credentials.conversation=links.source WHERE source=?1 AND target=?2 AND credentials.enabled=1)",
                params![transfer.source_id.to_string(),transfer.target_id.to_string()],|row|row.get(0))?;
            if !linked {
                self.transition(
                    &mut transfer,
                    TransferState::Cancelled,
                    Some("El vínculo fue revocado".to_owned()),
                )?;
                tx.commit()?;
                return Ok(None);
            }
        }
        let current = self.authorize(Caller::User, transfer.target_id)?;
        if current
            .provider
            .is_none_or(|p| p == crate::orchestration::AgentProvider::Unknown)
        {
            self.transition(
                &mut transfer,
                TransferState::NeedsReview,
                Some(
                    "El destino no tiene un proveedor registrado; no se envía contexto a un shell"
                        .into(),
                ),
            )?;
            tx.commit()?;
            return Ok(None);
        }
        if current.binding_id != Some(binding) || transfer.target_binding != Some(binding) {
            self.transition(
                &mut transfer,
                TransferState::NeedsReview,
                Some(
                    "Cambió la sesión de destino; revisá el contexto antes de reanudar".to_owned(),
                ),
            )?;
            tx.commit()?;
            return Ok(None);
        }
        self.transition(&mut transfer, TransferState::Dispatching, None)?;
        tx.commit()?;
        Ok(Some(transfer))
    }
    pub fn delivery_result(&self, id: Uuid, result: Result<(), String>) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut transfer = self.transfer(Caller::User, id)?;
        if transfer.state == TransferState::Completed {
            return Ok(());
        }
        anyhow::ensure!(
            transfer.state == TransferState::Dispatching,
            "La operación no está siendo entregada"
        );
        match result {
            Ok(()) => self.transition(&mut transfer, TransferState::InputQueued, None),
            Err(error) => self.transition(&mut transfer, TransferState::Failed, Some(error)),
        }?;
        if transfer.state == TransferState::InputQueued {
            let target = self.authorize(Caller::User, transfer.target_id)?;
            self.db.execute("INSERT INTO messages(id,conversation,role,text,source,created,origin) VALUES(?1,?2,?3,?4,'incoming_request',?5,?6)",
                params![Uuid::new_v4().to_string(),transfer.target_id.to_string(),serde_json::to_string(&MessageRole::User)?,transfer.prompt()?,chrono::Utc::now().timestamp_millis(),serde_json::to_string(&MessageOrigin::from(&target))?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn reply(&mut self, caller: Caller, id: Uuid, text: &str) -> Result<Transfer> {
        let tx = rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut transfer = self.transfer(caller, id)?;
        if let Some(own) = caller.agent_id() {
            anyhow::ensure!(
                own == transfer.target_id,
                "Sólo el destinatario puede responder la operación"
            );
        }
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= MAX_MESSAGE_BYTES,
            "Respuesta vacía o demasiado grande"
        );
        if transfer.state == TransferState::Completed {
            anyhow::ensure!(
                transfer.result.as_deref() == Some(text),
                "Ya hay otra respuesta para esta operación"
            );
            tx.commit()?;
            return Ok(transfer);
        }
        anyhow::ensure!(
            matches!(
                transfer.state,
                TransferState::Dispatching | TransferState::InputQueued
            ),
            "La operación no fue entregada"
        );
        let pending_children:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM transfers WHERE json_extract(data,'$.parent_id')=?1 AND state NOT IN ('completed','failed','cancelled'))",[id.to_string()],|row|row.get(0))?;
        anyhow::ensure!(!pending_children,"Todavía quedan operaciones delegadas sin resultado; esperá o resolvelas antes de responder");
        let previous = state_name(transfer.state);
        transfer.result = Some(text.to_owned());
        transfer.state = TransferState::Completed;
        transfer.error = None;
        let target = self.authorize(Caller::User, transfer.target_id)?;
        tx.execute("INSERT INTO messages(id,conversation,role,text,source,created,origin) VALUES(?1,?2,?3,?4,'peer_reply',?5,?6)",
            params![Uuid::new_v4().to_string(),transfer.target_id.to_string(),serde_json::to_string(&MessageRole::Assistant)?,text,chrono::Utc::now().timestamp_millis(),serde_json::to_string(&MessageOrigin::from(&target))?])?;
        let changed = tx.execute(
            "UPDATE transfers SET state='completed',data=?1 WHERE id=?2 AND state=?3",
            params![serde_json::to_string(&transfer)?, id.to_string(), previous],
        )?;
        anyhow::ensure!(
            changed == 1,
            "La operación cambió antes de guardar la respuesta"
        );
        tx.commit()?;
        Ok(transfer)
    }
    pub fn history(&self, caller: Caller, thread: Uuid, after: i64) -> Result<Vec<HistoryEntry>> {
        self.authorize(caller, thread)?;
        let mut query=self.db.prepare("SELECT id,sequence,role,substr(text,1,512),length(CAST(text AS BLOB)) FROM messages WHERE conversation=?1 AND sequence>?2 ORDER BY sequence LIMIT 50")?;
        let result = query
            .query_map(params![thread.to_string(), after], |row| {
                let role: String = row.get(2)?;
                let role = serde_json::from_str(&role).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(HistoryEntry {
                    id: uuid_column(row, 0)?,
                    sequence: row.get(1)?,
                    role,
                    preview: row.get(3)?,
                    bytes: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(result)
    }
    pub fn next_pending(&self, target: Uuid) -> Result<Option<Uuid>> {
        Ok(self.db.query_row("SELECT id FROM transfers WHERE target=?1 AND state='pending' AND NOT EXISTS(SELECT 1 FROM transfers WHERE target=?1 AND state IN ('dispatching','input_queued')) ORDER BY rowid LIMIT 1",[target.to_string()],|row|uuid_column(row,0)).optional()?)
    }
    pub fn list(
        &self,
        caller: Caller,
        workspace: Uuid,
        after: Option<Uuid>,
    ) -> Result<Vec<Conversation>> {
        if let Some(own) = caller.agent_id() {
            anyhow::ensure!(
                self.authorize(caller, own)?.workspace_id == workspace,
                "Workspace fuera del alcance"
            );
        }
        let mut query=self.db.prepare("SELECT data FROM conversations WHERE workspace=?1 AND id>?2 AND (?3 IS NULL OR id=?3 OR EXISTS(SELECT 1 FROM links WHERE source=?3 AND target=conversations.id)) ORDER BY id LIMIT 51")?;
        let own = caller.agent_id().map(|id| id.to_string());
        let result = query
            .query_map(
                params![
                    workspace.to_string(),
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    own
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result
            .iter()
            .map(|data| Ok(serde_json::from_str(data)?))
            .collect()
    }
    pub fn snapshot(&self, caller: Caller, workspace: Uuid) -> Result<HubSnapshot> {
        if let Some(own) = caller.agent_id() {
            anyhow::ensure!(
                self.authorize(caller, own)?.workspace_id == workspace,
                "Workspace fuera del alcance"
            );
        }
        let mut conversations = Vec::new();
        let mut query = self
            .db
            .prepare("SELECT data FROM conversations WHERE workspace=?1 ORDER BY id LIMIT 2000")?;
        for data in query.query_map([workspace.to_string()], |row| row.get::<_, String>(0))? {
            let value: Conversation = serde_json::from_str(&data?)?;
            if self.authorize(caller, value.id).is_ok() {
                conversations.push(value);
            }
        }
        let visible: std::collections::HashSet<_> = conversations.iter().map(|c| c.id).collect();
        let mut links = Vec::new();
        let mut query = self
            .db
            .prepare("SELECT source,target,automatic FROM links")?;
        for value in query.query_map([], |row| {
            Ok(ConversationLink {
                source_id: uuid_column(row, 0)?,
                target_id: uuid_column(row, 1)?,
                automatic_delivery: row.get(2)?,
            })
        })? {
            let link = value?;
            if visible.contains(&link.source_id) && visible.contains(&link.target_id) {
                links.push(link)
            }
        }
        let mut transfers = Vec::new();
        let mut query=self.db.prepare("SELECT id,source,target,json_extract(data,'$.state'),substr(json_extract(data,'$.request'),1,512),substr(json_extract(data,'$.result'),1,512),json_extract(data,'$.error') FROM transfers WHERE source IN (SELECT id FROM conversations WHERE workspace=?1)
            ORDER BY rowid DESC LIMIT 100")?;
        for value in query.query_map([workspace.to_string()], |row| {
            let state: String = row.get(3)?;
            let state = serde_json::from_value(serde_json::Value::String(state)).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(TransferSummary {
                id: uuid_column(row, 0)?,
                source_id: uuid_column(row, 1)?,
                target_id: uuid_column(row, 2)?,
                state,
                request_preview: row.get(4)?,
                result_preview: row.get(5)?,
                error: row.get(6)?,
            })
        })? {
            let transfer = value?;
            if matches!(caller, Caller::User)
                || caller
                    .agent_id()
                    .is_some_and(|own| own == transfer.source_id || own == transfer.target_id)
            {
                transfers.push(transfer)
            }
        }
        let mut connected = Vec::new();
        let mut query=self.db.prepare("SELECT DISTINCT bridges.conversation FROM bridges JOIN credentials USING(conversation) JOIN conversations ON conversations.id=bridges.conversation WHERE conversations.workspace=?1 AND bridges.digest=credentials.digest AND credentials.enabled=1 AND expires>?2")?;
        for id in query.query_map(
            params![workspace.to_string(), chrono::Utc::now().timestamp_millis()],
            |row| uuid_column(row, 0),
        )? {
            let id = id?;
            if visible.contains(&id) {
                connected.push(id);
            }
        }
        Ok(HubSnapshot {
            conversations,
            connected,
            links,
            transfers,
        })
    }
    pub fn attention(&self, targets: &[Uuid]) -> Result<Vec<Uuid>> {
        let mut query=self.db.prepare("SELECT id FROM transfers WHERE state IN ('awaiting_approval','needs_review') AND target IN (SELECT value FROM json_each(?1)) ORDER BY rowid DESC LIMIT 128")?;
        let result = query
            .query_map([serde_json::to_string(targets)?], |row| uuid_column(row, 0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(result)
    }
}

fn credential_matches(expected: &[u8], actual: &[u8]) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn state_name(state: TransferState) -> &'static str {
    match state {
        TransferState::AwaitingApproval => "awaiting_approval",
        TransferState::Pending => "pending",
        TransferState::Dispatching => "dispatching",
        TransferState::InputQueued => "input_queued",
        TransferState::NeedsReview => "needs_review",
        TransferState::Failed => "failed",
        TransferState::Completed => "completed",
        TransferState::Cancelled => "cancelled",
    }
}
fn measure_context(context: &mut ContextSnapshot) -> Result<()> {
    loop {
        let size = serde_json::to_vec(context)?.len();
        if size == context.used_bytes {
            return Ok(());
        }
        context.used_bytes = size;
    }
}
fn uuid_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(index)?;
    Uuid::parse_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn read_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationMessage> {
    let role: String = row.get(3)?;
    let role = serde_json::from_str(&role).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let origin: String = row.get(7)?;
    let origin = serde_json::from_str(&origin).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ConversationMessage {
        id: uuid_column(row, 0)?,
        sequence: row.get(1)?,
        conversation_id: uuid_column(row, 2)?,
        role,
        text: row.get(4)?,
        source: row.get(5)?,
        origin,
        created_at: row.get(6)?,
    })
}
