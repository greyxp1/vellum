use std::path::PathBuf;

use clap::CommandFactory;
use clap_complete::Shell;
use clap_complete_nushell::Nushell;

#[path = "src/bin/cli/mod.rs"]
mod cli;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/bin/cli/mod.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or(std::io::ErrorKind::NotFound)?);

    {
        let output = out_dir.join("man");
        if output.exists() {
            std::fs::remove_dir_all(&output)?;
        }
        std::fs::create_dir_all(&output)?;
        clap_mangen::generate_to(cli::Cli::command(), output)?;
    }

    {
        let output = out_dir.join("completions");
        if output.exists() {
            std::fs::remove_dir_all(&output)?;
        }
        std::fs::create_dir_all(&output)?;
        for shell in [
            Shell::Bash,
            Shell::Elvish,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Zsh,
        ] {
            clap_complete::generate_to(shell, &mut cli::Cli::command(), "vellum", &output)?;
        }
        clap_complete::generate_to(Nushell, &mut cli::Cli::command(), "vellum", &output)?;
    }

    Ok(())
}
