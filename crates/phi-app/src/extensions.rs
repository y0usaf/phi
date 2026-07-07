//! Extension host wiring for the bare core: discover, gate project-local
//! Lua on trust, load, and announce the session. The trust *prompt* lives
//! here — phi-host is headless and only remembers decisions.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result};
use phi_host::{Host, TrustDecision, TrustStore};

/// Discover and load extensions onto an existing host. With no extension
/// files present this is the zero-extension bare boot (doctrine 06) — the
/// host exists but nothing extension-shaped is registered. A failing
/// extension (including a name collision with an earlier registration) is
/// reported on stderr and skipped, never fatal.
pub fn load_discovered(host: &Host) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving cwd")?;
    let include_project = project_trusted(&cwd);
    let global_dir = phi_host::global_extensions_dir();
    for path in phi_host::discover_extensions(global_dir.as_deref(), &cwd, include_project) {
        if let Err(e) = host.load_file(&path) {
            eprintln!("phi: extension error: {e:#}");
        }
    }
    Ok(())
}

/// Emit an event and surface handler failures on stderr; handler results
/// are ignored here (the bare core has no consumer for them yet).
pub fn announce(host: &Host, event: &str, payload: serde_json::Value) {
    match host.emit(event, &payload) {
        Ok(outcomes) => {
            for outcome in outcomes {
                if let Err(e) = outcome.result {
                    eprintln!("phi: {} handler failed in {}: {e}", event, outcome.source);
                }
            }
        }
        Err(e) => eprintln!("phi: dispatching {event}: {e:#}"),
    }
}

/// Decide whether `.phi/extensions` may load: remembered decisions win;
/// otherwise prompt when interactive, skip when not.
fn project_trusted(cwd: &Path) -> bool {
    if !phi_host::project_extensions_present(cwd) {
        return false;
    }
    let Some(store_path) = TrustStore::default_path() else {
        return false;
    };
    let mut store = TrustStore::open(store_path);
    match store.decision(cwd) {
        TrustDecision::Trusted => true,
        TrustDecision::Denied => false,
        TrustDecision::Undecided => prompt_trust(&mut store, cwd),
    }
}

fn prompt_trust(store: &mut TrustStore, cwd: &Path) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!(
            "phi: {} has extensions but the project is untrusted; skipping (run interactively to decide)",
            phi_host::project_extensions_dir(cwd).display()
        );
        return false;
    }
    eprint!(
        "Load project extensions from {}? They run Lua with your full permissions.\n[y]es always / [o]nce / [N]o / n[e]ver: ",
        phi_host::project_extensions_dir(cwd).display()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => {
            if let Err(e) = store.set(cwd, true) {
                eprintln!("phi: could not persist trust decision: {e:#}");
            }
            true
        }
        "o" | "once" => true,
        "e" | "never" => {
            if let Err(e) = store.set(cwd, false) {
                eprintln!("phi: could not persist trust decision: {e:#}");
            }
            false
        }
        _ => false,
    }
}
