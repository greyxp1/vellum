use std::path::Path;

use clap::{CommandFactory, ValueEnum};
use clap_complete::aot::{Shell, generate_to};
use clap_complete_nushell::Nushell;

#[path = "src/bin/cli/mod.rs"]
mod cli;

pub(crate) fn generate(man_dir: &Path, completions_dir: &Path) -> std::io::Result<()> {
    for output in [man_dir, completions_dir] {
        if output.exists() {
            std::fs::remove_dir_all(output)?;
        }
        std::fs::create_dir_all(output)?;
    }
    let mut command = cli::Cli::command().name("vellum");
    clap_mangen::generate_to(command.clone(), man_dir)?;

    for &shell in Shell::value_variants() {
        generate_to(shell, &mut command, "vellum", completions_dir)?;
    }
    generate_to(Nushell, &mut command, "vellum", completions_dir)?;

    Ok(())
}
