mod font;
mod keybinds;
mod size;
mod watch;

pub(crate) use watch::Config;

use font::FontConfig;
pub(crate) use keybinds::{BindingAction, Keybinds, MouseButton};
pub(crate) use size::SizeRange;
use size::SizeRangeConfig;

use crate::Rgba;
pub(crate) use crate::cli::MIN_OPACITY;
use crate::draw::Style;
use crate::tool::{Icon, Primitive, ToolId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONFIG_FILE: &str = "vellum/config.toml";
const DEFAULT_TEXT_SIZE_STEP: f32 = 0.5;
const MAX_WHEEL_TOOLS: usize = 16;
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
    remember_tool_properties: Option<bool>,
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
    annotation_shadows: Option<bool>,
    annotation_shadow_overlap: Option<bool>,
    #[serde(default)]
    presentation: PresentationConfig,
    #[serde(default)]
    cursor: CursorVisibility,
    #[serde(default)]
    tools: BTreeMap<String, ToolConfig>,
    tool_wheel: Option<Vec<String>>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WhiteboardConfig {
    color: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PresentationConfig {
    remember: Option<bool>,
    hold_ms: Option<u64>,
    fade_duration_ms: Option<u64>,
    #[serde(default)]
    pen_effect: PenEffect,
    #[serde(default)]
    erase_duration_ms: EraseDuration,
    #[serde(default)]
    erase_pace: ErasePace,
}

#[derive(Debug, serde::Deserialize)]
#[serde(
    untagged,
    expecting = "\"drawn\" or an integer duration in milliseconds"
)]
enum EraseDuration {
    Fixed(u64),
    Drawn(DrawnDuration),
}

impl Default for EraseDuration {
    fn default() -> Self {
        Self::Drawn(DrawnDuration::Drawn)
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum DrawnDuration {
    Drawn,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ErasePace {
    Constant,
    #[default]
    Recorded,
}

#[derive(Clone, Copy)]
pub(crate) struct PresentationSettings {
    pub remember: bool,
    pub hold: Duration,
    pub fade_duration: Duration,
    pub pen_effect: PenEffect,
    pub erase_duration: Option<Duration>,
    pub erase_pace: ErasePace,
}

impl PresentationConfig {
    fn resolve(self) -> Result<PresentationSettings, String> {
        let hold_ms = self.hold_ms.unwrap_or(3000);
        if hold_ms > 60_000 {
            return Err("presentation.hold_ms must not exceed 60000".into());
        }
        let fade_duration_ms = self.fade_duration_ms.unwrap_or(1000);
        let erase_duration_ms = match self.erase_duration_ms {
            EraseDuration::Fixed(ms) => Some(ms),
            EraseDuration::Drawn(_) => None,
        };
        for (name, ms) in std::iter::once(("fade_duration_ms", fade_duration_ms))
            .chain(erase_duration_ms.map(|ms| ("erase_duration_ms", ms)))
        {
            if !(1..=60_000).contains(&ms) {
                return Err(format!("presentation.{name} must be between 1 and 60000"));
            }
        }
        Ok(PresentationSettings {
            remember: self.remember.unwrap_or(true),
            hold: Duration::from_millis(hold_ms),
            fade_duration: Duration::from_millis(fade_duration_ms),
            pen_effect: self.pen_effect,
            erase_duration: erase_duration_ms.map(Duration::from_millis),
            erase_pace: self.erase_pace,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PenEffect {
    #[default]
    Fade,
    Erase,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DrawOn {
    #[default]
    All,
    Current,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolConfig {
    primitive: Primitive,
    icon: Option<Icon>,
    size: Option<f32>,
    size_range: Option<SizeRangeConfig>,
    #[serde(default)]
    steps: StepConfig,
    #[serde(default)]
    stops: StopConfig,
    opacity: Option<f32>,
    roundness: Option<f32>,
    filled: Option<bool>,
    background: Option<bool>,
    font: Option<FontConfig>,
    #[serde(default)]
    cursor: CursorVisibility,
}

pub(crate) struct ToolDefinition {
    pub id: Arc<str>,
    pub primitive: Primitive,
    pub icon: Icon,
    pub properties: ToolProperties,
    pub size_range: SizeRange,
    pub steps: Steps,
    pub stops: Stops,
    pub font: Option<Arc<crate::text::TextFont>>,
    pub cursor: bool,
}

impl ToolDefinition {
    pub(crate) fn find(tools: &[Self], name: &str) -> Option<ToolId> {
        // Settings resolves the BTreeMap in name order.
        tools
            .binary_search_by(|tool| tool.id.as_ref().cmp(name))
            .ok()
            .map(ToolId)
    }

    pub(crate) fn preset(tools: &[Self], name: &str, primitive: Primitive) -> ToolId {
        Self::find(tools, name)
            .filter(|id| tools[id.0].primitive == primitive)
            .unwrap_or_else(|| Self::builtin(tools, primitive))
    }

    pub(crate) fn builtin(tools: &[Self], primitive: Primitive) -> ToolId {
        Self::find(tools, primitive.name()).expect("built-in tools are always registered")
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct ToolProperties {
    pub size: f32,
    pub opacity: f32,
    pub roundness: f32,
    pub filled: bool,
}

impl ToolProperties {
    pub(crate) fn style(self, [r, g, b]: [f32; 3]) -> Style {
        Style {
            size: self.size,
            color: [r, g, b, self.opacity],
            roundness: self.roundness,
            filled: self.filled,
        }
    }
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

pub(super) struct Settings {
    pub(super) keybinds: Keybinds,
    pub(super) text_keybinds: Keybinds,
    pub(super) draw_on: DrawOn,
    pub(super) tools: Arc<[ToolDefinition]>,
    pub(super) default_tool: ToolId,
    pub(super) tool_wheel: Vec<ToolId>,
    pub(super) remember_last_tool: bool,
    pub(super) remember_tool_properties: bool,
    pub(super) default_color: Rgba,
    pub(super) palette: Vec<Rgba>,
    pub(super) feedback_duration: Duration,
    pub(super) presentation: PresentationSettings,
    pub(super) annotation_shadows: bool,
    pub(super) annotation_shadow_overlap: bool,
    pub(super) clear_on_cancel: bool,
    pub(super) freeze_on_activate: bool,
    pub(super) annotations_visible: bool,
    pub(super) whiteboard_color: Rgba,
}

impl Settings {
    fn resolve(file: FileConfig) -> Result<Self, String> {
        let global_range = file
            .size_range
            .resolve(&SizeRange::default())
            .validate("size_range")?;
        let stroke_size = file.stroke_size.unwrap_or_else(|| global_range.clamp(5.0));
        if !global_range.contains(stroke_size) {
            return Err("stroke_size must be within size_range".into());
        }
        let palette = file
            .palette
            .unwrap_or_else(|| DEFAULT_PALETTE.iter().map(ToString::to_string).collect());
        if !(2..=12).contains(&palette.len()) {
            return Err("palette must contain between 2 and 12 colors".into());
        }
        let palette = palette
            .iter()
            .map(|value| crate::cli::visible_color(value).map(crate::color_to_srgb))
            .collect::<Result<Vec<_>, _>>()?;
        let default_color = file
            .default_color
            .as_deref()
            .map(crate::cli::visible_color)
            .transpose()?
            .map(crate::color_to_srgb)
            .unwrap_or(palette[0]);
        if !palette.contains(&default_color) {
            return Err("default_color must be present in palette".into());
        }
        let feedback_duration_ms = file.feedback_duration_ms.unwrap_or(500);
        if feedback_duration_ms > 60_000 {
            return Err("feedback_duration_ms must not exceed 60000".into());
        }
        let mut configs = file.tools;
        for primitive in Primitive::ALL {
            let config = configs
                .entry(primitive.name().into())
                .or_insert_with(|| ToolConfig {
                    primitive,
                    ..ToolConfig::default()
                });
            if config.primitive != primitive {
                let name = primitive.name();
                return Err(format!("tools.{name}.primitive must be {name:?}"));
            }
        }
        let global_size_stops = stops(
            "stops.size",
            file.stops.size.as_deref().unwrap_or_default(),
            global_range.min(),
            global_range.max(),
        )?;
        let global_opacity_stops = stops(
            "stops.opacity",
            file.stops.opacity.as_deref().unwrap_or_default(),
            MIN_OPACITY,
            1.0,
        )?;
        let global_roundness_stops = stops(
            "stops.roundness",
            file.stops.roundness.as_deref().unwrap_or_default(),
            0.0,
            1.0,
        )?;
        let global_steps = Steps {
            size: valid_step("steps.size", file.steps.size.unwrap_or(1.0))?,
            opacity: valid_step("steps.opacity", file.steps.opacity.unwrap_or(0.01))?,
            roundness: valid_step("steps.roundness", file.steps.roundness.unwrap_or(0.01))?,
        };
        let mut tools = Vec::with_capacity(configs.len());
        for (id, config) in configs {
            crate::cli::tool_name(&id).map_err(|error| format!("tools.{id}: {error}"))?;
            let prefix = format!("tools.{id}");
            let primitive = config.primitive;
            let built_in = id == primitive.name();
            let built_in_text = built_in && primitive == Primitive::Text;
            for (field, configured, supported) in [
                (
                    "size",
                    config.size.is_some(),
                    primitive != Primitive::Select,
                ),
                (
                    "size_range",
                    config.size_range.is_some(),
                    primitive != Primitive::Select,
                ),
                (
                    "steps.size",
                    config.steps.size.is_some(),
                    primitive != Primitive::Select,
                ),
                (
                    "stops.size",
                    config.stops.size.is_some(),
                    primitive != Primitive::Select,
                ),
                (
                    "opacity",
                    config.opacity.is_some(),
                    primitive.supports_opacity(),
                ),
                (
                    "steps.opacity",
                    config.steps.opacity.is_some(),
                    primitive.supports_opacity(),
                ),
                (
                    "stops.opacity",
                    config.stops.opacity.is_some(),
                    primitive.supports_opacity(),
                ),
                (
                    "roundness",
                    config.roundness.is_some(),
                    primitive.supports_roundness(),
                ),
                (
                    "steps.roundness",
                    config.steps.roundness.is_some(),
                    primitive.supports_roundness(),
                ),
                (
                    "stops.roundness",
                    config.stops.roundness.is_some(),
                    primitive.supports_roundness(),
                ),
                ("filled", config.filled.is_some(), primitive.supports_fill()),
                (
                    "background",
                    config.background.is_some(),
                    primitive == Primitive::Text,
                ),
                ("font", config.font.is_some(), primitive == Primitive::Text),
            ] {
                if configured && !supported {
                    return Err(format!(
                        "{prefix}.{field} is not supported by {}",
                        primitive.name()
                    ));
                }
            }
            if !built_in {
                for (field, missing) in [
                    ("icon", config.icon.is_none()),
                    (
                        "roundness",
                        primitive.supports_roundness() && config.roundness.is_none(),
                    ),
                    (
                        "background",
                        primitive == Primitive::Text && config.background.is_none(),
                    ),
                ] {
                    if missing {
                        return Err(format!("{prefix}.{field} is required for custom tools"));
                    }
                }
            }
            let fallback_range = if built_in_text {
                file.size_range.resolve(&SizeRange::text())
            } else {
                global_range
            };
            let range = config
                .size_range
                .as_ref()
                .map_or(fallback_range, |value| value.resolve(&fallback_range))
                .validate(&format!("{prefix}.size_range"))?;
            let size = config.size.unwrap_or_else(|| {
                range.clamp(match (built_in, primitive) {
                    (true, Primitive::Text) => 16.0,
                    (true, Primitive::Eraser) => 10.0,
                    _ => stroke_size,
                })
            });
            if !range.contains(size) {
                return Err(format!("{prefix}.size must be within its size_range"));
            }
            let opacity = config.opacity.unwrap_or(default_color[3]);
            if !(MIN_OPACITY..=1.0).contains(&opacity) {
                return Err(format!(
                    "{prefix}.opacity must be between {MIN_OPACITY} and 1"
                ));
            }
            let roundness = config.roundness.unwrap_or(match primitive {
                Primitive::Pen => 1.0,
                Primitive::Line => 0.5,
                Primitive::Arrow => 0.25,
                Primitive::Rectangle | Primitive::Text => 0.1,
                _ => 0.0,
            });
            if !roundness.is_finite() || !(0.0..=1.0).contains(&roundness) {
                return Err(format!("{prefix}.roundness must be between 0 and 1"));
            }
            let steps = Steps {
                size: valid_step(
                    &format!("{prefix}.steps.size"),
                    config
                        .steps
                        .size
                        .unwrap_or(if built_in_text && file.steps.size.is_none() {
                            DEFAULT_TEXT_SIZE_STEP
                        } else {
                            global_steps.size
                        }),
                )?,
                opacity: valid_step(
                    &format!("{prefix}.steps.opacity"),
                    config.steps.opacity.unwrap_or(global_steps.opacity),
                )?,
                roundness: valid_step(
                    &format!("{prefix}.steps.roundness"),
                    config.steps.roundness.unwrap_or(global_steps.roundness),
                )?,
            };
            let stop_set = Stops {
                size: match config.stops.size {
                    Some(values) => stops(
                        &format!("{prefix}.stops.size"),
                        &values,
                        range.min(),
                        range.max(),
                    )?,
                    None => global_size_stops
                        .iter()
                        .copied()
                        .filter(|v| range.contains(*v))
                        .collect(),
                },
                opacity: match config.stops.opacity {
                    Some(values) => stops(
                        &format!("{prefix}.stops.opacity"),
                        &values,
                        MIN_OPACITY,
                        1.0,
                    )?,
                    None => global_opacity_stops.clone(),
                },
                roundness: match config.stops.roundness {
                    Some(values) => stops(&format!("{prefix}.stops.roundness"), &values, 0.0, 1.0)?,
                    None => global_roundness_stops.clone(),
                },
            };
            let font = (primitive == Primitive::Text)
                .then(|| {
                    config
                        .font
                        .unwrap_or_default()
                        .resolve(&prefix, built_in)
                        .map(Arc::new)
                })
                .transpose()?;
            tools.push(ToolDefinition {
                id: id.into(),
                primitive,
                icon: config.icon.unwrap_or_else(|| primitive.into()),
                properties: ToolProperties {
                    size,
                    opacity,
                    roundness,
                    filled: if primitive == Primitive::Text {
                        config.background.unwrap_or(false)
                    } else {
                        config.filled.unwrap_or(
                            file.default_fill_shapes.unwrap_or(false) && primitive.supports_fill(),
                        )
                    },
                },
                size_range: range,
                steps,
                stops: stop_set,
                font,
                cursor: config.cursor.visible.or(file.cursor.visible).unwrap_or(
                    !built_in
                        || !matches!(
                            primitive,
                            Primitive::Pen | Primitive::Arrow | Primitive::Eraser
                        ),
                ),
            });
        }
        let resolve_id = |id: &str| {
            ToolDefinition::find(&tools, id).ok_or_else(|| format!("unknown tool {id:?}"))
        };
        let default = file.default_tool.as_deref().unwrap_or("pen");
        let default_tool = resolve_id(&default.to_ascii_lowercase())
            .map_err(|error| format!("default_tool: {error}"))?;
        let wheel = file.tool_wheel.unwrap_or_else(|| {
            [
                "pen",
                "line",
                "arrow",
                "triangle",
                "rectangle",
                "ellipse",
                "select",
                "text",
            ]
            .into_iter()
            .map(String::from)
            .collect()
        });
        if !(1..=MAX_WHEEL_TOOLS).contains(&wheel.len()) {
            return Err(format!(
                "tool_wheel must contain between 1 and {MAX_WHEEL_TOOLS} tools"
            ));
        }
        let mut tool_wheel = Vec::with_capacity(wheel.len());
        for name in wheel {
            let id = resolve_id(&name).map_err(|error| format!("tool_wheel: {error}"))?;
            if tool_wheel.contains(&id) {
                return Err(format!("tool_wheel contains duplicate tool {name:?}"));
            }
            tool_wheel.push(id);
        }
        Ok(Self {
            keybinds: Keybinds::resolve(file.keybinds, false)?,
            text_keybinds: Keybinds::resolve(file.text_keybinds, true)?,
            draw_on: file.draw_on.unwrap_or_default(),
            tools: tools.into(),
            default_tool,
            tool_wheel,
            remember_last_tool: file.remember_last_tool.unwrap_or(true),
            remember_tool_properties: file.remember_tool_properties.unwrap_or(true),
            default_color,
            palette,
            feedback_duration: Duration::from_millis(feedback_duration_ms),
            presentation: file.presentation.resolve()?,
            annotation_shadows: file.annotation_shadows.unwrap_or(true),
            annotation_shadow_overlap: file.annotation_shadow_overlap.unwrap_or(false),
            clear_on_cancel: file.clear_on_cancel.unwrap_or(false),
            freeze_on_activate: file.freeze_on_activate.unwrap_or(false),
            annotations_visible: file.annotations_visible.unwrap_or(true),
            whiteboard_color: crate::color_to_srgb(
                crate::cli::opaque_color(file.whiteboard.color.as_deref().unwrap_or("#EBEBEB"))
                    .map_err(|error| format!("whiteboard.color: {error}"))?,
            ),
        })
    }
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
