use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Option<Command>,

    /// Read this TOML preferences file
    #[arg(long, conflicts_with = "no_config")]
    pub(super) config: Option<PathBuf>,

    /// Ignore preferences files
    #[arg(long)]
    pub(super) no_config: bool,

    /// Force the rendering backend
    #[arg(short = 'b', long, value_name = "BACKEND")]
    pub(super) force_backend: Option<Backend>,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// Switch drawing mode
    Toggle,
    /// Activate drawing mode
    Activate,
    /// Deactivate drawing mode
    Deactivate,
    /// Clear annotations
    Clear,
    /// Clear annotations and deactivate drawing mode
    ClearAndDeactivate,
    /// Report whether drawing mode is active
    IsActive,
    /// Stop Vellum
    Exit,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Backend {
    Vulkan,
    #[value(name = "opengl", alias = "gl")]
    OpenGL,
}
