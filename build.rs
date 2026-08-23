use std::path::PathBuf;

use clap::{CommandFactory, ValueEnum};
use clap_complete::aot::{Shell, generate_to};
use clap_complete_nushell::Nushell;

#[path = "src/bin/cli/mod.rs"]
mod cli;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/bin/cli/mod.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or(std::io::ErrorKind::NotFound)?);
    let man_dir = out_dir.join("man");
    let completions_dir = out_dir.join("completions");

    for output in [&man_dir, &completions_dir] {
        if output.exists() {
            std::fs::remove_dir_all(output)?;
        }
        std::fs::create_dir_all(output)?;
    }
    clap_mangen::generate_to(cli::Cli::command(), man_dir)?;

    let mut command = cli::Cli::command();
    for &shell in Shell::value_variants() {
        generate_to(shell, &mut command, "vellum", &completions_dir)?;
    }
    generate_to(Nushell, &mut command, "vellum", completions_dir)?;

    Ok(())
}
