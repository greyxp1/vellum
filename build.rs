use std::io::Write;
use std::path::PathBuf;

use clap::{CommandFactory, ValueEnum};
use clap_complete::aot::{Shell, generate_to};
use clap_complete_nushell::Nushell;

#[path = "src/cli.rs"]
mod cli;
#[allow(dead_code)]
#[path = "src/tool.rs"]
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
    let command = cli::Cli::command();
    clap_mangen::generate_to(command.clone(), &man_dir)?;
    let tool_names = tool::Primitive::ALL.map(tool::Primitive::name);
    let mut command = command.mut_subcommands(|command| {
        command.mut_args(|arg| {
            if arg.get_id() == "tool" {
                arg.value_parser(clap::builder::PossibleValuesParser::new(tool_names))
            } else {
                arg
            }
        })
    });
    for &shell in Shell::value_variants() {
        generate_to(shell, &mut command, "vellum", &completions_dir)?;
    }
    generate_to(Nushell, &mut command, "vellum", &completions_dir)?;
    // The Fish generator emits option values but skips positional arguments.
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(completions_dir.join("vellum.fish"))?,
        "complete -c vellum -n '__fish_vellum_using_subcommand use; and test (count (commandline -opc)) -eq 2' -f -a '{}'",
        tool_names.join(" "),
    )?;
    Ok(())
}
