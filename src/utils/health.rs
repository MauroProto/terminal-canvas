//! Read-only package/profile inspection. Does not open a window, shell, network
//! connection or database, so support can check a freshly extracted package.

pub fn check() -> anyhow::Result<serde_json::Value> {
    let paths = super::app_paths::get()?;
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Executable has no parent directory"))?;
    let mut names = vec!["tc-memory", "tc-memory-mcp"];
    if cfg!(all(unix, feature = "daemon")) {
        names.push("mi-terminal-daemon");
    }
    let helpers: Vec<_> = names
        .into_iter()
        .map(|name| {
            let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
            serde_json::json!({ "name": filename, "present": directory.join(&filename).is_file() })
        })
        .collect();
    let complete = helpers.iter().all(|helper| helper["present"] == true);
    Ok(serde_json::json!({
        "ok": complete,
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "daemon": cfg!(all(unix, feature = "daemon")),
        "profile": {
            "isolated": paths.isolated_root.is_some(),
            "config": paths.config,
            "data": paths.data,
            "cache": paths.cache,
            "panic_log": super::app_paths::panic_log_path(),
            "global_agent_configuration": super::app_paths::permits_global_agent_configuration(),
        },
        "helpers": helpers,
    }))
}
