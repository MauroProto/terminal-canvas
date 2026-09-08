//! CLI de diagnóstico y fallback para proveedores sin MCP.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match mi_terminal::memory::run_cli(&args) {
        Ok(out) => {
            print!("{out}");
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
