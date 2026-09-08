//! Bridge MCP stdio sobre el Memory Hub local.

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(err) = mi_terminal::memory::serve_mcp_stdio() {
        eprintln!("tc-memory-mcp: {err}");
        std::process::exit(1);
    }
}
