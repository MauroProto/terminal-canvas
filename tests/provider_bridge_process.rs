//! Provider launch configuration exercised through the real stdio executable.
//! No provider account, model request, GUI, or user configuration is required.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use mi_terminal::conversation::{launch, Caller, Conversation, ConversationStore};
use mi_terminal::orchestration::AgentProvider;
use serde_json::{json, Value};
use uuid::Uuid;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tc-provider profile-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Interpret the configuration passed to the provider, rather than constructing
// a separate test-only MCP command that could diverge from a real launch.
fn bridge_command(provider: AgentProvider, config: &launch::McpConfig) -> Command {
    let executable = Path::new(env!("CARGO_BIN_EXE_mi-terminal"));
    let mut provider_command = Command::new("unused-provider-fixture");
    launch::append_config(&mut provider_command, provider, config, executable).unwrap();
    let args = provider_command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect::<Vec<_>>();
    let (program, bridge_args, auth) = match provider {
        AgentProvider::ClaudeCode => {
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], "--mcp-config");
            let value: Value = serde_json::from_slice(&std::fs::read(args[1]).unwrap()).unwrap();
            let server = &value["mcpServers"]["terminal_canvas_chat"];
            (
                server["command"].as_str().unwrap().to_owned(),
                serde_json::from_value::<Vec<String>>(server["args"].clone()).unwrap(),
                server["env"]["TC_CHAT_AUTH_FILE"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        }
        AgentProvider::CodexCli => {
            assert_eq!(args.len() % 2, 0);
            let mut overrides = BTreeMap::new();
            for pair in args.chunks_exact(2) {
                assert_eq!(pair[0], "-c");
                let (key, value) = pair[1].split_once('=').unwrap();
                overrides.insert(
                    key,
                    toml::from_str::<toml::Table>(&format!("value={value}")).unwrap()["value"]
                        .clone(),
                );
            }
            (
                overrides["mcp_servers.terminal_canvas_chat.command"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                overrides["mcp_servers.terminal_canvas_chat.args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|arg| arg.as_str().unwrap().to_owned())
                    .collect(),
                overrides["mcp_servers.terminal_canvas_chat.env.TC_CHAT_AUTH_FILE"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        }
        _ => panic!("Unexpected provider fixture"),
    };
    assert_eq!(Path::new(&program), executable);
    assert_eq!(bridge_args, ["--chat-mcp"]);
    assert_eq!(Path::new(&auth), config.auth_path);
    let mut command = Command::new(program);
    command.args(bridge_args).env("TC_CHAT_AUTH_FILE", auth);
    command
}

struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Result<Value, String>>,
    stderr: PathBuf,
    next_id: u64,
}
impl Bridge {
    fn start(mut command: Command, scratch: &Scratch, label: &str) -> Self {
        let stderr = scratch.0.join(format!("{label}.stderr"));
        let log = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stderr)
            .unwrap();
        let mut child = command
            .current_dir(&scratch.0)
            .env("TERMINAL_CANVAS_HOME", &scratch.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let parsed = line.map_err(|error| error.to_string()).and_then(|line| {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                });
                if tx.send(parsed).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            replies,
            stderr,
            next_id: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let message = json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params});
        let stdin = self.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &message).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        let reply = self
            .replies
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], self.next_id);
        reply
    }

    fn initialize(&mut self) {
        let reply = self.request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"provider-process-regression","version":"1"}}));
        assert_eq!(
            reply["result"]["serverInfo"]["name"],
            "terminal-canvas-chat"
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))["result"].clone()
    }

    fn finish(mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "MCP exit failed: {}",
                    std::fs::read_to_string(&self.stderr).unwrap()
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "MCP did not stop after stdin EOF"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn conversation(id: Uuid, workspace: Uuid, provider: AgentProvider) -> Conversation {
    Conversation {
        id,
        workspace_id: workspace,
        panel_id: Uuid::new_v4(),
        title: "Provider fixture".into(),
        provider: Some(provider),
        cwd: None,
        memory_task_id: None,
        native_session_id: None,
        runtime_session_id: None,
        binding_id: None,
    }
}

fn read_text(bridge: &mut Bridge, id: Uuid) -> String {
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let reply = bridge.call(
            "tc_chat_read",
            json!({"message_id":id,"offset":offset,"limit":32000}),
        );
        assert_eq!(reply["isError"], false);
        let value = &reply["structuredContent"];
        assert_eq!(value["offset"], offset);
        text.push_str(value["text"].as_str().unwrap());
        let Some(next) = value["next_offset"].as_u64() else {
            return text;
        };
        assert!(next > offset);
        offset = next;
    }
}

#[test]
fn claude_and_codex_launch_configs_reach_a_durable_scoped_stdio_bridge() {
    for provider in [AgentProvider::ClaudeCode, AgentProvider::CodexCli] {
        let scratch = Scratch::new();
        let database = scratch.0.join("data/conversations.sqlite3");
        let store = ConversationStore::open(&database).unwrap();
        let own = conversation(Uuid::new_v4(), Uuid::new_v4(), provider);
        let unrelated = conversation(Uuid::new_v4(), own.workspace_id, provider);
        let outside = conversation(Uuid::new_v4(), Uuid::new_v4(), provider);
        for row in [&own, &unrelated, &outside] {
            store.register(row).unwrap();
        }
        let config = launch::issue_config(
            &store,
            own.id,
            &database,
            &scratch.0.join("private MCP"),
            Path::new(env!("CARGO_BIN_EXE_mi-terminal")),
        )
        .unwrap();
        let mut bridge = Bridge::start(bridge_command(provider, &config), &scratch, "first");
        assert_eq!(
            bridge.request("tools/list", json!({}))["error"]["code"],
            -32000
        );
        bridge.initialize();
        let list = bridge.call("tc_chat_list", json!({}));
        assert_eq!(list["isError"], false);
        assert_eq!(
            list["structuredContent"]["conversations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(list["structuredContent"]["own_id"], own.id.to_string());
        assert_eq!(
            bridge.call("tc_chat_ready", json!({"available":true}))["isError"],
            false
        );
        assert!(store.bridge_connected(own.id).unwrap());
        assert_eq!(store.bridge_availability(own.id).unwrap(), Some(true));
        let id = Uuid::new_v4();
        let text = "á🚀".repeat(10000);
        assert_eq!(
            bridge.call(
                "tc_chat_record",
                json!({"message_id":id,"role":"assistant","text":text})
            )["isError"],
            false
        );
        assert_eq!(read_text(&mut bridge, id), text);
        for inaccessible in [unrelated.id, outside.id] {
            assert_eq!(
                bridge.call("tc_chat_read", json!({"conversation_id":inaccessible}))["isError"],
                true
            );
        }
        bridge.finish();
        assert!(!store.bridge_connected(own.id).unwrap());

        let mut restarted = Bridge::start(bridge_command(provider, &config), &scratch, "restarted");
        restarted.initialize();
        assert_eq!(read_text(&mut restarted, id), text);
        // Rotation invalidates a live process immediately, without waiting for
        // its lease timeout or trusting cached initialization credentials.
        store.issue_credential(own.id).unwrap();
        assert_eq!(
            restarted.call("tc_chat_read", json!({"message_id":id}))["isError"],
            true
        );
        assert!(!store.bridge_connected(own.id).unwrap());
        restarted.finish();

        let refreshed = launch::issue_config(
            &store,
            own.id,
            &database,
            &scratch.0.join("private MCP"),
            Path::new(env!("CARGO_BIN_EXE_mi-terminal")),
        )
        .unwrap();
        let mut final_bridge =
            Bridge::start(bridge_command(provider, &refreshed), &scratch, "refreshed");
        final_bridge.initialize();
        assert_eq!(read_text(&mut final_bridge, id), text);
        store.revoke(own.id).unwrap();
        assert_eq!(
            final_bridge.call(
                "tc_chat_record",
                json!({"message_id":Uuid::new_v4(),"role":"assistant","text":"must not persist"})
            )["isError"],
            true
        );
        final_bridge.finish();
        let reopened = ConversationStore::open(&database).unwrap();
        assert_eq!(reopened.history(Caller::User, own.id, 0).unwrap().len(), 1);
        assert!(reopened
            .history(Caller::User, unrelated.id, 0)
            .unwrap()
            .is_empty());
        assert!(reopened
            .history(Caller::User, outside.id, 0)
            .unwrap()
            .is_empty());
    }
}
