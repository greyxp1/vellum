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
        #[arg(
            long = "target",
            value_enum,
            hide_possible_values = true,
            next_line_help = true,
            long_help = tool_values_help()
        )]
        tool: Option<Tool>,
        /// Output JSON
        #[arg(long)]
        json: bool,
        /// Test silently: 0=true, 1=false or Vellum not running, 2=error
        #[arg(long, requires = "field", conflicts_with = "json")]
        check: bool,
    },
    /// Select a tool
    Use {
        #[arg(
            value_enum,
            hide_possible_values = true,
            next_line_help = true,
            long_help = tool_values_help()
        )]
        tool: Tool,
    },
    /// Change tool settings
    Set {
        #[arg(
            long = "target",
            value_enum,
            global = true,
            hide_possible_values = true,
            next_line_help = true,
            long_help = tool_values_help()
        )]
        tool: Option<Tool>,
        #[command(subcommand)]
        setting: ToolSetting,
    },
    /// Toggle drawing mode
    Toggle,
    /// Activate drawing mode
    Activate,
    /// Deactivate drawing mode
    Deactivate,
    /// Clear annotations
    Clear {
        /// Deactivate after clearing
        #[arg(long)]
        deactivate: bool,
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

fn tool_values_help() -> String {
    use clap::ValueEnum;

    let mut help = String::from("Possible values:");
    for tool in Tool::value_variants() {
        help.push_str("\n- ");
        help.push_str(tool.name());
    }
    help
}

#[derive(clap::Subcommand)]
pub(crate) enum ToolSetting {
    /// Size in logical pixels
    Size {
        #[arg(value_parser = positive_size)]
        value: f32,
    },
    /// Opacity (0.05 to 1)
    Opacity {
        #[arg(value_parser = opacity)]
        value: f32,
    },
    /// Roundness (0 to 1)
    Roundness {
        #[arg(value_parser = roundness)]
        value: f32,
    },
    /// Shape fill or text background
    Filled {
        #[arg(action = clap::ArgAction::Set)]
        value: bool,
    },
    /// CSS color
    Color {
        #[arg(value_parser = visible_color)]
        value: DynamicColor,
    },
}

fn number_in_range(value: &str, min: f32, max: f32) -> Result<f32, String> {
    let number: f32 = value.parse().map_err(|_| "expected a number")?;
    if !number.is_finite() || !(min..=max).contains(&number) {
        return Err(format!("value must be between {min} and {max}"));
    }
    Ok(number)
}

pub(crate) fn positive_size(value: &str) -> Result<f32, String> {
    let number: f32 = value.parse().map_err(|_| "expected a size")?;
    if !number.is_finite() || number <= 0.0 {
        return Err("size must be finite and positive".into());
    }
    Ok(number)
}

pub(crate) fn opacity(value: &str) -> Result<f32, String> {
    number_in_range(value, MIN_OPACITY, 1.0)
}

pub(crate) fn roundness(value: &str) -> Result<f32, String> {
    number_in_range(value, 0.0, 1.0)
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
    /// Show desktop annotations when inactive
    Show,
    /// Hide desktop annotations when inactive
    Hide,
    /// Toggle desktop annotations when inactive
    Toggle,
}

#[derive(clap::Subcommand)]
pub(crate) enum WhiteboardCommand {
    /// Enter whiteboard mode
    Show,
    /// Return to desktop mode
    Hide,
    /// Switch between desktop and whiteboard modes
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
    if !color.components.iter().all(|value| value.is_finite()) {
        return Err("color components must be finite".into());
    }
    if color.components[3] < MIN_OPACITY {
        return Err(format!("color opacity must be at least {MIN_OPACITY}"));
    }
    Ok(color)
}
