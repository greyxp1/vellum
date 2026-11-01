use std::path::PathBuf;

use crate::tool::Tool;
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
    /// Show drawing status
    Status {
        /// Show one field. Unsupported properties return null.
        #[arg(value_enum)]
        field: Option<StatusField>,
        /// Show settings for a tool
        #[arg(long, value_enum)]
        tool: Option<Tool>,
        /// Output JSON
        #[arg(long)]
        json: bool,
        /// Test silently: 0=true, 1=false or Vellum not running, 2=error
        #[arg(long, requires = "field", conflicts_with = "json")]
        check: bool,
    },
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

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum StatusField {
    /// Drawing is active
    Active,
    /// Drawing mode: inactive, desktop or whiteboard
    Mode,
    /// Whiteboard is active
    Whiteboard,
    /// Tool name
    Tool,
    /// Drawing color (sRGB)
    Color,
    /// Size in logical pixels
    Size,
    /// Opacity (0 to 1)
    Opacity,
    /// Roundness (0 to 1)
    Roundness,
    /// Shape fill or text background
    Filled,
    /// Editing a text annotation
    TextEditing,
    /// Keep annotations visible when inactive
    AnnotationsVisible,
    /// Whiteboard background color (sRGB)
    WhiteboardColor,
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
