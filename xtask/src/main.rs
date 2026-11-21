use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Vellum project tasks")]
struct Arguments {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Build and install Vellum, its manual, and shell completions
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
    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "install",
            "--locked",
            "--message-format=json-render-diagnostics",
            "--path",
        ])
        .arg(workspace)
        .arg("--root")
        .arg(&root)
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(format!("cargo install failed with {}", output.status).into());
    }

    let messages = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()?;
    let package = messages
        .iter()
        .find(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == "vellum"
        })
        .and_then(|message| message["package_id"].as_str())
        .ok_or("cargo did not report the Vellum artifact")?;
    let generated = messages
        .iter()
        .find(|message| {
            message["reason"] == "build-script-executed" && message["package_id"] == package
        })
        .and_then(|message| message["out_dir"].as_str())
        .map(Path::new)
        .ok_or("cargo did not report Vellum's documentation directory")?;
    let man_dir = generated.join("man");
    let completions_dir = generated.join("completions");

    for entry in fs::read_dir(man_dir)? {
        let source = entry?.path();
        let section = source
            .extension()
            .and_then(|extension| extension.to_str())
            .ok_or("manual page has no section")?;
        install(
            &source,
            &root
                .join("share/man")
                .join(format!("man{section}"))
                .join(file_name(&source)?),
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
