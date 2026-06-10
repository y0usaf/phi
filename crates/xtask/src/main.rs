mod generate_models;

fn main() -> anyhow::Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("generate-models") => generate_models::run(),
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- <task>\n\ntasks:\n  generate-models    regenerate crates/phi-ai/src/models.json from models.dev"
            );
            std::process::exit(2);
        }
    }
}
