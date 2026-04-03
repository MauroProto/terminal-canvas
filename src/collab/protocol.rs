use std::fmt;

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use super::models::{
    ControlGrant, ControlRequest, ControlRevoke, GuestId, GuestPresence, GuestTerminalInput,
    InviteCode, JoinDecision, JoinRequest, ParticipantId, SessionRole, ShareSessionId,
    SharedWorkspaceSnapshot, TrustedDevice,
};

const INVITE_PREFIX: &str = "terminalcanvas://join/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateShareSessionRequest {
    pub session_secret: String,
    pub invite_secret: String,
    #[serde(default)]
    pub invite_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub passphrase_hash: Option<String>,
    #[serde(default)]
    pub trusted_devices: Vec<TrustedDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateShareSessionResponse {
    pub session_id: ShareSessionId,
    pub host_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinShareSessionRequest {
    pub display_name: String,
    pub invite_secret: String,
    pub device_id: String,
    #[serde(default)]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinShareSessionResponse {
    pub guest_id: GuestId,
    pub guest_token: String,
    #[serde(default)]
    pub auto_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinDecisionRequest {
    pub host_token: String,
    pub guest_id: GuestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndShareSessionRequest {
    pub host_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateInviteRequest {
    pub host_token: String,
    pub invite_secret: String,
    #[serde(default)]
    pub invite_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerControlMessage {
    Connected {
        role: SessionRole,
        guest_id: Option<GuestId>,
    },
    JoinRequested {
        request: JoinRequest,
    },
    JoinApproved {
        decision: JoinDecision,
    },
    JoinDenied {
        decision: JoinDecision,
    },
    Presence {
        guests: Vec<GuestPresence>,
    },
    HostDisconnected,
    HostReconnected,
    SessionEnded,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionPayload {
    WorkspaceSnapshot { snapshot: SharedWorkspaceSnapshot },
    ControlRequest { request: ControlRequest },
    ControlGrant { grant: ControlGrant },
    ControlRevoke { revoke: ControlRevoke },
    GuestInput { input: GuestTerminalInput },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollabEnvelope {
    pub session_id: ShareSessionId,
    pub sender_id: ParticipantId,
    pub message_seq: u64,
    pub nonce: String,
    pub encrypted_payload: String,
}

#[derive(Debug)]
pub enum ProtocolError {
    InvalidInvitePrefix,
    InvalidInviteEncoding,
    InvalidInviteJson,
    InvalidKeyLength,
    InvalidNonce,
    EncryptFailed,
    DecryptFailed,
    DecodeFailed,
    EncodeFailed,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInvitePrefix => write!(f, "invalid invite prefix"),
            Self::InvalidInviteEncoding => write!(f, "invalid invite encoding"),
            Self::InvalidInviteJson => write!(f, "invalid invite payload"),
            Self::InvalidKeyLength => write!(f, "invalid session secret length"),
            Self::InvalidNonce => write!(f, "invalid nonce"),
            Self::EncryptFailed => write!(f, "failed to encrypt payload"),
            Self::DecryptFailed => write!(f, "failed to decrypt payload"),
            Self::DecodeFailed => write!(f, "failed to decode payload"),
            Self::EncodeFailed => write!(f, "failed to encode payload"),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn encode_invite_code(invite: &InviteCode) -> Result<String, ProtocolError> {
    let bytes = serde_json::to_vec(invite).map_err(|_| ProtocolError::InvalidInviteJson)?;
    Ok(format!(
        "{INVITE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn decode_invite_code(value: &str) -> Result<InviteCode, ProtocolError> {
    let trimmed = value.trim();
    let encoded = trimmed
        .strip_prefix(INVITE_PREFIX)
        .ok_or(ProtocolError::InvalidInvitePrefix)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ProtocolError::InvalidInviteEncoding)?;
    serde_json::from_slice(&bytes).map_err(|_| ProtocolError::InvalidInviteJson)
}

pub fn invite_code_from_cli_args<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let arg = arg.as_ref().trim();
        if let Some(value) = arg.strip_prefix("--join=") {
            if let Some(invite) = normalize_invite_candidate(value) {
                return Some(invite);
            }
            continue;
        }
        if arg == "--join" {
            if let Some(next) = iter.next() {
                if let Some(invite) = normalize_invite_candidate(next.as_ref()) {
                    return Some(invite);
                }
            }
            continue;
        }
        if let Some(invite) = normalize_invite_candidate(arg) {
            return Some(invite);
        }
    }
    None
}

pub fn invite_code_from_launch_sources<I, S>(args: I, env_invite: Option<String>) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    invite_code_from_cli_args(args)
        .or_else(|| env_invite.and_then(|value| normalize_invite_candidate(&value)))
}

fn normalize_invite_candidate(value: &str) -> Option<String> {
    let trimmed = value.trim();
    decode_invite_code(trimmed).ok()?;
    Some(trimmed.to_owned())
}

pub fn encode_envelope(
    session_id: ShareSessionId,
    sender_id: ParticipantId,
    message_seq: u64,
    session_secret_b64: &str,
    payload: &SessionPayload,
) -> Result<CollabEnvelope, ProtocolError> {
    let key_bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(session_secret_b64)
        .map_err(|_| ProtocolError::InvalidKeyLength)?;
    if key_bytes.len() != 32 {
        return Err(ProtocolError::InvalidKeyLength);
    }

    let cipher = XChaCha20Poly1305::new_from_slice(&key_bytes)
        .map_err(|_| ProtocolError::InvalidKeyLength)?;
    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let encoded_payload =
        rmp_serde::to_vec_named(payload).map_err(|_| ProtocolError::EncodeFailed)?;
    let ciphertext = cipher
        .encrypt(nonce, encoded_payload.as_ref())
        .map_err(|_| ProtocolError::EncryptFailed)?;

    Ok(CollabEnvelope {
        session_id,
        sender_id,
        message_seq,
        nonce: base64::engine::general_purpose::STANDARD_NO_PAD.encode(nonce_bytes),
        encrypted_payload: base64::engine::general_purpose::STANDARD_NO_PAD.encode(ciphertext),
    })
}

pub fn decode_envelope(
    envelope: &CollabEnvelope,
    session_secret_b64: &str,
) -> Result<SessionPayload, ProtocolError> {
    let key_bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(session_secret_b64)
        .map_err(|_| ProtocolError::InvalidKeyLength)?;
    if key_bytes.len() != 32 {
        return Err(ProtocolError::InvalidKeyLength);
    }
    let nonce_bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&envelope.nonce)
        .map_err(|_| ProtocolError::InvalidNonce)?;
    if nonce_bytes.len() != 24 {
        return Err(ProtocolError::InvalidNonce);
    }
    let ciphertext = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&envelope.encrypted_payload)
        .map_err(|_| ProtocolError::DecryptFailed)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key_bytes)
        .map_err(|_| ProtocolError::InvalidKeyLength)?;
    let plaintext = cipher
        .decrypt(XNonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| ProtocolError::DecryptFailed)?;
    rmp_serde::from_slice(&plaintext).map_err(|_| ProtocolError::DecodeFailed)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::collab::models::{GuestConnectionState, GuestId, GuestPresence};

    fn session_secret() -> String {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(key)
    }

    #[test]
    fn invite_code_round_trip() {
        let invite = InviteCode {
            broker_url: "https://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: session_secret(),
            invite_secret: Some("invite-secret".to_owned()),
            expires_at: Some(Utc::now()),
            requires_passphrase: false,
            tls_cert_pem: None,
        };

        let encoded = encode_invite_code(&invite).unwrap();
        let decoded = decode_invite_code(&encoded).unwrap();

        assert_eq!(decoded, invite);
    }

    #[test]
    fn collab_envelope_round_trip() {
        let payload = SessionPayload::WorkspaceSnapshot {
            snapshot: SharedWorkspaceSnapshot {
                workspace_id: Uuid::new_v4(),
                workspace_name: "Demo".to_owned(),
                generated_at: Utc::now(),
                guests: vec![GuestPresence {
                    id: GuestId(Uuid::new_v4()),
                    display_name: "Mauro".to_owned(),
                    joined_at: Utc::now(),
                    connection_state: GuestConnectionState::Connected,
                }],
                terminal_controls: Vec::new(),
                panels: Vec::new(),
            },
        };
        let secret = session_secret();
        let envelope = encode_envelope(
            ShareSessionId(Uuid::new_v4()),
            ParticipantId::Host,
            1,
            &secret,
            &payload,
        )
        .unwrap();

        let decoded = decode_envelope(&envelope, &secret).unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn invite_code_is_found_in_cli_args() {
        let invite = InviteCode {
            broker_url: "https://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: session_secret(),
            invite_secret: Some("invite-secret".to_owned()),
            expires_at: None,
            requires_passphrase: false,
            tls_cert_pem: None,
        };
        let encoded = encode_invite_code(&invite).unwrap();
        let args = vec!["mi-terminal".to_owned(), encoded.clone()];
        assert_eq!(invite_code_from_cli_args(args), Some(encoded));
    }

    #[test]
    fn invite_code_is_found_in_join_flag_forms() {
        let invite = InviteCode {
            broker_url: "https://127.0.0.1:8787".to_owned(),
            session_id: ShareSessionId(Uuid::new_v4()),
            session_secret: session_secret(),
            invite_secret: Some("invite-secret".to_owned()),
            expires_at: None,
            requires_passphrase: false,
            tls_cert_pem: None,
        };
        let encoded = encode_invite_code(&invite).unwrap();
        assert_eq!(
            invite_code_from_cli_args(vec![
                "mi-terminal".to_owned(),
                "--join".to_owned(),
                encoded.clone()
            ]),
            Some(encoded.clone())
        );
        assert_eq!(
            invite_code_from_cli_args(vec!["mi-terminal".to_owned(), format!("--join={encoded}")]),
            Some(encoded)
        );
    }
}
