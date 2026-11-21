use std::io::Write;
use std::path::{Path, PathBuf};

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
    write_config_manual(&man_dir.join("vellum.5"))?;
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

fn write_config_manual(path: &Path) -> std::io::Result<()> {
    let mut man = std::fs::File::create(path)?;
    writeln!(
        man,
        ".TH VELLUM 5\n.SH NAME\nvellum \\- configuration file for the Vellum screen annotation overlay"
    )?;
    // Render the reference's headings, paragraphs and table rows as man paragraphs.
    for (page, section) in [
        ("Configuration", "CONFIG FILE"),
        ("Global-settings", "GLOBAL SETTINGS"),
        ("Per-tool-settings", "PER-TOOL SETTINGS"),
        ("Keybinds", "KEYBINDS"),
    ] {
        let path = format!("docs/config/{page}.md");
        println!("cargo:rerun-if-changed={path}");
        writeln!(man, ".SH {section}")?;
        let markdown = std::fs::read_to_string(path)?;
        let mut columns = None;
        for line in markdown.lines() {
            if let Some(row) = line.strip_prefix('|').and_then(|row| row.strip_suffix('|')) {
                let mut cells = row.split('|').map(str::trim);
                if columns.is_none() {
                    columns = Some(cells.skip(1).collect::<Vec<_>>());
                } else if let Some(term) = cells.next().filter(|term| !term.starts_with("---")) {
                    writeln!(man, ".TP\n\\fB{}\\fR", man_text(term))?;
                    for (label, value) in columns.as_deref().unwrap_or_default().iter().zip(cells) {
                        if value.is_empty() || (*label == "Supported primitives" && value == "All")
                        {
                            continue;
                        }
                        let text = man_text(value);
                        if matches!(
                            *label,
                            "What it changes" | "Supported values" | "Default bindings"
                        ) {
                            writeln!(man, "\\&{text}")?;
                        } else {
                            let period = if text.ends_with('.') { "" } else { "." };
                            writeln!(man, "\\fB{label}:\\fR {text}{period}")?;
                        }
                    }
                }
            } else {
                columns = None;
                if let Some(heading) = line.strip_prefix("### ") {
                    writeln!(man, ".SS {}", man_text(heading))?;
                } else if let Some((number, item)) = line.split_once(". ").filter(|(number, _)| {
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
                }) {
                    writeln!(man, ".IP \"{number}.\" 3\n\\&{}", man_text(item))?;
                } else if line.is_empty() {
                    writeln!(man, ".PP")?;
                } else if !line.starts_with("## ") {
                    writeln!(man, "\\&{}", man_text(line))?;
                }
            }
        }
    }
    writeln!(
        man,
        ".SH SEE ALSO\n.BR vellum (1)\n.PP\nhttps://github.com/greyxp1/vellum/wiki/Examples"
    )
}

fn man_text(markdown: &str) -> String {
    let mut text = markdown
        .replace('\\', "\\e")
        .replace('-', "\\-")
        .replace('`', "")
        .replace("<kbd>", "")
        .replace("</kbd>", "")
        .replace(".<br>", ". ")
        .replace("<br>", ", ");
    while let Some((start, middle, end)) = text.find("](").and_then(|middle| {
        let start = text[..middle].rfind('[')?;
        let end = middle + 2 + text[middle + 2..].find(')')?;
        Some((start, middle, end))
    }) {
        text.replace_range(middle..=end, "");
        text.remove(start);
    }
    text
}
