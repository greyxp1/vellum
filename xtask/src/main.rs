use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Parser, Subcommand};

#[path = "../../build_support.rs"]
mod build_support;

#[derive(Parser)]
#[command(about = "Vellum project tasks")]
struct Arguments {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Build and install Vellum, its default config, manual, and shell completions
    Install {
        /// Installation prefix (default: ~/.local)
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Task::Install { root } = Arguments::parse().task;
    let root = root.map_or_else(default_root, Ok)?;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask is not inside the Vellum workspace")?;
    let generated = workspace.join("target/xtask");
    let man_dir = generated.join("man");
    let completions_dir = generated.join("completions");
    build_support::generate(&man_dir, &completions_dir)?;

    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["install", "--locked", "--path"])
        .arg(workspace)
        .arg("--root")
        .arg(&root)
        .status()?;
    if !status.success() {
        return Err(format!("cargo install failed with {status}").into());
    }

    for entry in fs::read_dir(man_dir)? {
        let source = entry?.path();
        install(
            &source,
            &root.join("share/man/man1").join(file_name(&source)?),
        )?;
    }

    for (source, destination) in [
        (
            "vellum.bash",
            "share/bash-completion/completions/vellum.bash",
        ),
        ("vellum.elv", "share/elvish/lib/vellum.elv"),
        ("vellum.fish", "share/fish/vendor_completions.d/vellum.fish"),
        ("vellum.nu", "share/nushell/vendor/autoload/vellum.nu"),
        ("_vellum.ps1", "share/powershell/vellum.Completion.ps1"),
        ("_vellum", "share/zsh/site-functions/_vellum"),
    ] {
        install(&completions_dir.join(source), &root.join(destination))?;
    }
    install(
        &workspace.join("default-config.toml"),
        &root.join("share/doc/vellum/default-config.toml"),
    )?;

    println!(
        "Installed Vellum and its documentation to {}",
        root.display()
    );
    Ok(())
}

fn default_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?).join(".local"))
}

fn file_name(path: &Path) -> Result<&std::ffi::OsStr, Box<dyn std::error::Error>> {
    path.file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()).into())
}

fn install(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(
        destination
            .parent()
            .ok_or(std::io::ErrorKind::InvalidInput)?,
    )?;
    std::fs::copy(source, destination)?;
    Ok(())
}
