use std::path::PathBuf;

use clap::{CommandFactory, ValueEnum};
use clap_complete::aot::{Shell, generate_to};
use clap_complete_nushell::Nushell;

#[path = "src/cli.rs"]
mod cli;

#[path = "src/tool.rs"]
#[allow(dead_code)]
mod tool;

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=src/tool.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or(std::io::ErrorKind::NotFound)?);
    let man_dir = out_dir.join("man");
    let completions_dir = out_dir.join("completions");
    for output in [&man_dir, &completions_dir] {
        if output.exists() {
            std::fs::remove_dir_all(output)?;
        }
        std::fs::create_dir_all(output)?;
    }
    let mut command = cli::Cli::command();
    // Man pages need native value lists instead of the terminal's multiline help.
    let man_command = command.clone().mut_subcommands(|command| {
        command.mut_args(|arg| {
            if arg.get_id() == "tool" {
                arg.long_help(None).hide_possible_values(false)
            } else {
                arg
            }
        })
    });
    clap_mangen::generate_to(man_command, &man_dir)?;
    for &shell in Shell::value_variants() {
        generate_to(shell, &mut command, "vellum", &completions_dir)?;
    }
    generate_to(Nushell, &mut command, "vellum", &completions_dir)?;
    Ok(())
}
