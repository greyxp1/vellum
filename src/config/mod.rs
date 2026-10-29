mod font;
mod keybinds;
mod size;
mod watch;

pub(crate) use watch::Config;

use font::FontConfig;
pub(crate) use keybinds::{BindingAction, Keybinds, MouseButton};
pub(crate) use size::SizeRange;
use size::{SizeRangeConfig, resolve_size_ranges};

use crate::Rgba;
pub(crate) use crate::cli::MIN_OPACITY;
use crate::tool::Tool;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONFIG_FILE: &str = "vellum/config.toml";
const DEFAULT_TEXT_SIZE_STEP: f32 = 0.5;
const DEFAULT_PALETTE: [&str; 8] = [
    "#E84046", "#EF8F4F", "#EED14D", "#4DD54F", "#0483FA", "#7C58EA", "#EBEBEB", "#141414",
];

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    keybinds: keybinds::KeybindConfig,
    #[serde(default)]
    text_keybinds: keybinds::KeybindConfig,
    draw_on: Option<DrawOn>,
    default_tool: Option<String>,
    remember_last_tool: Option<bool>,
    stroke_size: Option<f32>,
    #[serde(default)]
    size_range: SizeRangeConfig,
    #[serde(default)]
    steps: StepConfig,
    #[serde(default)]
    stops: StopConfig,
    default_color: Option<String>,
    palette: Option<Vec<String>>,
    feedback_duration_ms: Option<u64>,
    clear_on_cancel: Option<bool>,
    freeze_on_activate: Option<bool>,
    annotations_visible: Option<bool>,
    #[serde(default)]
    whiteboard: WhiteboardConfig,
    default_fill_shapes: Option<bool>,
    #[serde(default)]
    cursor: CursorVisibility,
    #[serde(default)]
    tools: ToolDefaults,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WhiteboardConfig {
    color: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DrawOn {
    #[default]
    All,
    Current,
}

pub(crate) type ToolDefaults = BTreeMap<Tool, PropertyDefaults>;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PropertyDefaults {
    pub(crate) size: Option<f32>,
    size_range: Option<SizeRangeConfig>,
    #[serde(default)]
    steps: StepConfig,
    #[serde(default)]
    stops: StopConfig,
    pub(crate) opacity: Option<f32>,
    pub(crate) roundness: Option<f32>,
    pub(crate) filled: Option<bool>,
    pub(crate) background: Option<bool>,
    font: Option<FontConfig>,
    #[serde(default)]
    pub(crate) cursor: CursorVisibility,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CursorVisibility {
    pub(crate) visible: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StepConfig {
    size: Option<f32>,
    opacity: Option<f32>,
    roundness: Option<f32>,
}

#[derive(Clone, Copy)]
pub(crate) struct Steps {
    pub(crate) size: f32,
    pub(crate) opacity: f32,
    pub(crate) roundness: f32,
}

fn valid_step(name: &str, value: f32) -> Result<f32, String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{name} must be greater than 0"));
    }
    Ok(value)
}

fn resolve_steps(
    tools: &ToolDefaults,
    global: &StepConfig,
) -> Result<BTreeMap<Tool, Steps>, String> {
    let global_steps = Steps {
        size: valid_step("steps.size", global.size.unwrap_or(1.0))?,
        opacity: valid_step("steps.opacity", global.opacity.unwrap_or(0.01))?,
        roundness: valid_step("steps.roundness", global.roundness.unwrap_or(0.01))?,
    };
    Tool::SIZED
        .into_iter()
        .map(|tool| {
            let configured = tools.get(&tool).map(|defaults| &defaults.steps);
            let size = match configured.and_then(|steps| steps.size) {
                Some(value) => valid_step(&format!("tools.{}.steps.size", tool.name()), value)?,
                None if tool == Tool::Text && global.size.is_none() => DEFAULT_TEXT_SIZE_STEP,
                None => global_steps.size,
            };
            let opacity = match configured.and_then(|steps| steps.opacity) {
                Some(value) => valid_step(&format!("tools.{}.steps.opacity", tool.name()), value)?,
                None => global_steps.opacity,
            };
            let roundness = match configured.and_then(|steps| steps.roundness) {
                Some(value) => {
                    valid_step(&format!("tools.{}.steps.roundness", tool.name()), value)?
                }
                None => global_steps.roundness,
            };
            Ok((
                tool,
                Steps {
                    size,
                    opacity,
                    roundness,
                },
            ))
        })
        .collect()
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StopConfig {
    size: Option<Vec<f32>>,
    opacity: Option<Vec<f32>>,
    roundness: Option<Vec<f32>>,
}

#[derive(Clone)]
pub(crate) struct Stops {
    pub(crate) size: Arc<[f32]>,
    pub(crate) opacity: Arc<[f32]>,
    pub(crate) roundness: Arc<[f32]>,
}

fn stops(name: &str, values: &[f32], min: f32, max: f32) -> Result<Arc<[f32]>, String> {
    if values
        .iter()
        .any(|value| !value.is_finite() || !(min..=max).contains(value))
    {
        return Err(format!("{name} values must be between {min} and {max}"));
    }
    let mut values = values.to_vec();
    values.sort_by(f32::total_cmp);
    values.dedup();
    Ok(Arc::from(values))
}

fn resolve_stops(
    tools: &ToolDefaults,
    global: &StopConfig,
    global_size_range: &SizeRange,
    size_ranges: &BTreeMap<Tool, SizeRange>,
) -> Result<BTreeMap<Tool, Stops>, String> {
    let global_size = stops(
        "stops.size",
        global.size.as_deref().unwrap_or_default(),
        global_size_range.min(),
        global_size_range.max(),
    )?;
    let global_opacity = stops(
        "stops.opacity",
        global.opacity.as_deref().unwrap_or_default(),
        MIN_OPACITY,
        1.0,
    )?;
    let global_roundness = stops(
        "stops.roundness",
        global.roundness.as_deref().unwrap_or_default(),
        0.0,
        1.0,
    )?;
    Tool::SIZED
        .into_iter()
        .map(|tool| {
            let configured = tools.get(&tool).map(|defaults| &defaults.stops);
            let range = size_ranges
                .get(&tool)
                .expect("adjustable tools have size ranges");
            let size = match configured.and_then(|stops| stops.size.as_deref()) {
                Some(values) => stops(
                    &format!("tools.{}.stops.size", tool.name()),
                    values,
                    range.min(),
                    range.max(),
                )?,
                None => Arc::from(
                    global_size
                        .iter()
                        .copied()
                        .filter(|stop| range.contains(*stop))
                        .collect::<Vec<_>>(),
                ),
            };
            let opacity = match configured.and_then(|stops| stops.opacity.as_deref()) {
                Some(values) => stops(
                    &format!("tools.{}.stops.opacity", tool.name()),
                    values,
                    MIN_OPACITY,
                    1.0,
                )?,
                None => global_opacity.clone(),
            };
            let roundness = match configured.and_then(|stops| stops.roundness.as_deref()) {
                Some(values) => stops(
                    &format!("tools.{}.stops.roundness", tool.name()),
                    values,
                    0.0,
                    1.0,
                )?,
                None => global_roundness.clone(),
            };
            Ok((
                tool,
                Stops {
                    size,
                    opacity,
                    roundness,
                },
            ))
        })
        .collect()
}

fn validate_tool_defaults(
    tools: &ToolDefaults,
    size_ranges: &BTreeMap<Tool, SizeRange>,
) -> Result<(), String> {
    for (&tool, defaults) in tools {
        let prefix = format!("tools.{}", tool.name());
        let size = tool != Tool::Select;
        let opacity = tool.supports_opacity();
        let roundness = tool.supports_roundness();
        let text = tool == Tool::Text;
        let settings = [
            ("size", defaults.size.is_some(), size),
            ("size_range", defaults.size_range.is_some(), size),
            ("steps.size", defaults.steps.size.is_some(), size),
            ("stops.size", defaults.stops.size.is_some(), size),
            ("opacity", defaults.opacity.is_some(), opacity),
            ("steps.opacity", defaults.steps.opacity.is_some(), opacity),
            ("stops.opacity", defaults.stops.opacity.is_some(), opacity),
            ("roundness", defaults.roundness.is_some(), roundness),
            (
                "steps.roundness",
                defaults.steps.roundness.is_some(),
                roundness,
            ),
            (
                "stops.roundness",
                defaults.stops.roundness.is_some(),
                roundness,
            ),
            ("filled", defaults.filled.is_some(), tool.supports_fill()),
            ("background", defaults.background.is_some(), text),
        ];
        if let Some((name, ..)) = settings
            .iter()
            .find(|(_, set, supported)| *set && !supported)
        {
            return Err(format!("{prefix}.{name} is not supported"));
        }
        if let Some(value) = defaults.size
            && let Some(range) = size_ranges.get(&tool)
            && !range.contains(value)
        {
            return Err(format!(
                "{prefix}.size must be between {} and {}",
                range.min(),
                range.max(),
            ));
        }
        if let Some(value) = defaults.opacity
            && (!value.is_finite() || !(MIN_OPACITY..=1.0).contains(&value))
        {
            return Err(format!(
                "{prefix}.opacity must be between {MIN_OPACITY} and 1"
            ));
        }
        if let Some(value) = defaults.roundness
            && (!value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(format!("{prefix}.roundness must be between 0 and 1"));
        }
        if defaults.font.is_some() && !text {
            return Err(format!("{prefix} does not support font settings"));
        }
    }
    Ok(())
}

pub(super) struct Settings {
    pub(super) keybinds: Keybinds,
    pub(super) text_keybinds: Keybinds,
    pub(super) draw_on: DrawOn,
    pub(super) stroke_size: f32,
    pub(super) size_ranges: Arc<BTreeMap<Tool, SizeRange>>,
    pub(super) steps: Arc<BTreeMap<Tool, Steps>>,
    pub(super) stops: Arc<BTreeMap<Tool, Stops>>,
    pub(super) default_color: Rgba,
    pub(super) default_tool: Tool,
    pub(super) remember_last_tool: bool,
    pub(super) palette: Vec<Rgba>,
    pub(super) feedback_duration: Duration,
    pub(super) clear_on_cancel: bool,
    pub(super) freeze_on_activate: bool,
    pub(super) annotations_visible: bool,
    pub(super) whiteboard_color: Rgba,
    pub(super) default_fill_shapes: bool,
    pub(super) tool_defaults: ToolDefaults,
    pub(super) cursor: CursorVisibility,
    pub(super) text_font: crate::text::TextFont,
}

impl Settings {
    fn resolve(file: FileConfig) -> Result<Self, String> {
        let size_range = file
            .size_range
            .resolve(&SizeRange::default())
            .validate("size_range")?;
        let stroke_size = match file.stroke_size {
            Some(size) if !size_range.contains(size) => {
                return Err(format!(
                    "stroke_size must be between {} and {}",
                    size_range.min(),
                    size_range.max(),
                ));
            }
            Some(size) => size,
            None => size_range.clamp(5.0),
        };

        let default_tool = file
            .default_tool
            .unwrap_or_else(|| "pen".into())
            .to_ascii_lowercase()
            .parse()?;

        let palette_text = file
            .palette
            .unwrap_or_else(|| DEFAULT_PALETTE.iter().map(ToString::to_string).collect());
        if !(2..=12).contains(&palette_text.len()) {
            return Err("palette must contain between 2 and 12 colors".into());
        }
        let palette = palette_text
            .iter()
            .enumerate()
            .map(|(index, color)| {
                let name = format!("palette[{index}]");
                let rgba = parse_named_color(&name, color)?;
                if rgba[3] < MIN_OPACITY {
                    return Err(format!("{name} opacity must be at least {MIN_OPACITY}"));
                }
                Ok(rgba)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let default_color = match file.default_color {
            Some(color) => {
                let color = parse_named_color("default_color", &color)?;
                if !palette.contains(&color) {
                    return Err("default_color must be present in palette".into());
                }
                color
            }
            None => palette[0],
        };

        let feedback_duration_ms = file.feedback_duration_ms.unwrap_or(500);
        if feedback_duration_ms > 60_000 {
            return Err("feedback_duration_ms must not exceed 60000".into());
        }

        let size_ranges = Arc::new(resolve_size_ranges(
            &file.tools,
            &size_range,
            &file.size_range,
        )?);
        validate_tool_defaults(&file.tools, &size_ranges)?;
        let steps = Arc::new(resolve_steps(&file.tools, &file.steps)?);
        let stops = Arc::new(resolve_stops(
            &file.tools,
            &file.stops,
            &size_range,
            &size_ranges,
        )?);
        let text_font = file
            .tools
            .get(&Tool::Text)
            .and_then(|defaults| defaults.font.as_ref())
            .unwrap_or(&FontConfig::default())
            .resolve()?;

        Ok(Self {
            keybinds: Keybinds::resolve(file.keybinds, false)?,
            text_keybinds: Keybinds::resolve(file.text_keybinds, true)?,
            draw_on: file.draw_on.unwrap_or_default(),
            stroke_size,
            size_ranges,
            steps,
            stops,
            default_color,
            default_tool,
            remember_last_tool: file.remember_last_tool.unwrap_or(true),
            palette,
            feedback_duration: Duration::from_millis(feedback_duration_ms),
            clear_on_cancel: file.clear_on_cancel.unwrap_or(false),
            freeze_on_activate: file.freeze_on_activate.unwrap_or(false),
            annotations_visible: file.annotations_visible.unwrap_or(true),
            whiteboard_color: crate::color_to_srgb(
                crate::cli::opaque_color(file.whiteboard.color.as_deref().unwrap_or("#EBEBEB"))
                    .map_err(|error| format!("whiteboard.color: {error}"))?,
            ),
            default_fill_shapes: file.default_fill_shapes.unwrap_or(false),
            tool_defaults: file.tools,
            cursor: file.cursor,
            text_font,
        })
    }
}

fn parse_named_color(name: &str, value: &str) -> Result<Rgba, String> {
    value
        .parse()
        .map(crate::color_to_srgb)
        .map_err(|error| format!("invalid {name} {value:?}: {error}"))
}

fn read_config(path: &Path) -> Result<FileConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    parse_config(path, &contents)
}

fn read_optional_config(path: &Path) -> Result<Option<FileConfig>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_config(path, &contents).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn parse_config(path: &Path, contents: &str) -> Result<FileConfig, String> {
    toml::from_str(contents).map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn read_first_config(paths: impl IntoIterator<Item = PathBuf>) -> Result<FileConfig, String> {
    for path in paths {
        if let Some(config) = read_optional_config(&path)? {
            return Ok(config);
        }
    }
    Ok(FileConfig::default())
}

fn default_config_paths() -> Vec<PathBuf> {
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        });
    let mut paths: Vec<_> = user
        .into_iter()
        .map(|path| path.join(CONFIG_FILE))
        .collect();

    match std::env::var_os("XDG_CONFIG_DIRS").filter(|value| !value.is_empty()) {
        Some(dirs) => paths.extend(
            std::env::split_paths(&dirs)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(CONFIG_FILE)),
        ),
        None => paths.push(PathBuf::from("/etc/xdg").join(CONFIG_FILE)),
    }
    paths
}
