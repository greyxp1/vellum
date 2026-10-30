use std::path::PathBuf;

use color::DynamicColor;

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Option<Command>,

    /// Use this config file
    #[arg(long, conflicts_with = "no_config")]
    pub(super) config: Option<PathBuf>,

    /// Ignore config files
    #[arg(long)]
    pub(super) no_config: bool,

    /// Print the annotated default configuration and exit
    #[arg(long, conflicts_with_all = ["config", "no_config"])]
    pub(super) print_default_config: bool,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// Toggle drawing mode
    Toggle,
    /// Activate drawing mode
    Activate,
    /// Deactivate drawing mode
    Deactivate,
    /// Clear annotations
    Clear,
    /// Clear annotations and deactivate drawing mode
    ClearAndDeactivate,
    /// Set the drawing color
    SetColor {
        /// CSS color, such as `red`, `#00ffff`, or `oklab(0.5 0.2 0)`
        ///
        /// Quote colors containing #, spaces, or parentheses.
        #[arg(value_parser = visible_color)]
        color: DynamicColor,
    },
    /// Report whether drawing mode is active
    IsActive,
    /// Report whether text is being edited
    IsTextEditing,
    /// Report whether the whiteboard is active
    IsWhiteboard,
    /// Manage the whiteboard
    Whiteboard {
        #[command(subcommand)]
        command: WhiteboardCommand,
    },
    /// Manage desktop annotations
    Annotations {
        #[command(subcommand)]
        command: VisibilityCommand,
    },
    /// Stop Vellum
    Exit,
}

#[derive(clap::Subcommand)]
pub(crate) enum VisibilityCommand {
    /// Show desktop annotations
    Show,
    /// Hide desktop annotations
    Hide,
    /// Toggle desktop annotations
    Toggle,
}

#[derive(clap::Subcommand)]
pub(crate) enum WhiteboardCommand {
    /// Show the board
    Show,
    /// Hide the board
    Hide,
    /// Toggle the board
    Toggle,
    /// Clear the board's annotations
    Clear,
    /// Set the board's background color
    SetColor {
        /// Opaque CSS color, such as `white`, `#EBEBEB`, or `oklab(0.5 0.2 0)`
        #[arg(value_parser = opaque_color)]
        color: DynamicColor,
    },
}

pub(crate) fn opaque_color(value: &str) -> Result<DynamicColor, String> {
    let color: DynamicColor = value
        .parse()
        .map_err(|error| format!("invalid color: {error}"))?;
    if color.components[3] != 1.0 {
        return Err("whiteboard color must be opaque".into());
    }
    Ok(color)
}

pub(crate) const MIN_OPACITY: f32 = 0.05;

pub(crate) fn visible_color(value: &str) -> Result<DynamicColor, String> {
    let color: DynamicColor = value
        .parse()
        .map_err(|error| format!("invalid color: {error}"))?;
    if color.components[3] < MIN_OPACITY {
        return Err(format!("color opacity must be at least {MIN_OPACITY}"));
    }
    Ok(color)
}
