mod font;
mod icon;
mod keybinds;
mod size;
mod watch;

pub(crate) use icon::{SvgCommand, SvgIcon, ToolIcon};
pub(crate) use watch::Config;

use font::FontConfig;
pub(crate) use keybinds::{BindingAction, Gesture, Input, Keybinds, MouseButton};
pub(crate) use size::SizeRange;
use size::SizeRangeConfig;

use crate::Rgba;
pub(crate) use crate::cli::MIN_OPACITY;
use crate::draw::Style;
use crate::tool::{Primitive, ToolId};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const CONFIG_FILE: &str = "vellum/config.toml";
const DEFAULT_TEXT_SIZE_STEP: f32 = 0.5;
const MAX_WHEEL_TOOLS: usize = 16;
const DEFAULT_PALETTE: [&str; 8] = [
    "#E84046", "#EF8F4F", "#EED14D", "#4DD54F", "#0483FA", "#7C58EA", "#EBEBEB", "#141414",
];

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    #[serde(skip)]
    directory: PathBuf,
    keybinds: keybinds::KeybindConfig,
    text_keybinds: keybinds::KeybindConfig,
    draw_on: DrawOn,
    default_tool: Option<String>,
    remember_last_tool: Option<bool>,
    remember_tool_properties: Option<bool>,
    stroke_size: Option<f32>,
    size_range: SizeRangeConfig,
    steps: PerProperty<Option<f32>>,
    stops: PerProperty<Option<Vec<f32>>>,
    default_color: Option<String>,
    palette: Option<Vec<String>>,
    feedback_duration_ms: Option<u64>,
    clear_on_cancel: bool,
    freeze_on_activate: bool,
    annotations_visible: Option<bool>,
    whiteboard: WhiteboardConfig,
    default_fill_shapes: bool,
    annotation_shadows: Option<bool>,
    annotation_shadow_overlap: bool,
    presentation: PresentationConfig,
    cursor: CursorVisibility,
    tools: BTreeMap<String, ToolConfig>,
    tool_wheel: Option<Vec<String>>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WhiteboardConfig {
    color: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PresentationConfig {
    remember: Option<bool>,
    hold_ms: Option<u64>,
    fade_duration_ms: Option<u64>,
    pen_effect: PenEffect,
    erase_duration_ms: Option<EraseDuration>,
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

fn duration(name: &str, ms: u64, min: u64) -> Result<Duration, String> {
    if (min..=60_000).contains(&ms) {
        Ok(Duration::from_millis(ms))
    } else if min == 0 {
        Err(format!("{name} must not exceed 60000"))
    } else {
        Err(format!("{name} must be between {min} and 60000"))
    }
}

impl PresentationConfig {
    fn resolve(self) -> Result<PresentationSettings, String> {
        let name = |field| format!("presentation.{field}");
        Ok(PresentationSettings {
            remember: self.remember.unwrap_or(true),
            hold: duration(&name("hold_ms"), self.hold_ms.unwrap_or(3000), 0)?,
            fade_duration: duration(
                &name("fade_duration_ms"),
                self.fade_duration_ms.unwrap_or(1000),
                1,
            )?,
            pen_effect: self.pen_effect,
            erase_duration: match self.erase_duration_ms {
                Some(EraseDuration::Fixed(ms)) => {
                    Some(duration(&name("erase_duration_ms"), ms, 1)?)
                }
                Some(EraseDuration::Drawn(_)) | None => None,
            },
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
    icon: Option<icon::IconConfig>,
    size: Option<f32>,
    size_range: Option<SizeRangeConfig>,
    #[serde(default)]
    steps: PerProperty<Option<f32>>,
    #[serde(default)]
    stops: PerProperty<Option<Vec<f32>>>,
    opacity: Option<f32>,
    roundness: Option<f32>,
    filled: Option<bool>,
    background: Option<bool>,
    font: Option<FontConfig>,
    #[serde(default)]
    cursor: CursorVisibility,
}

impl ToolConfig {
    fn value(&self, property: Property) -> Option<f32> {
        match property {
            Property::Size => self.size,
            Property::Opacity => self.opacity,
            Property::Roundness => self.roundness,
        }
    }
}

pub(crate) struct ToolDefinition {
    pub id: Arc<str>,
    pub primitive: Primitive,
    pub icon: ToolIcon,
    pub properties: ToolProperties,
    pub size_range: SizeRange,
    pub steps: PerProperty<f32>,
    pub stops: PerProperty<Arc<[f32]>>,
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

/// A numeric tool property with a value, step size and stops.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Property {
    Size,
    Opacity,
    Roundness,
}

impl Property {
    const ALL: [Self; 3] = [Self::Size, Self::Opacity, Self::Roundness];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Opacity => "opacity",
            Self::Roundness => "roundness",
        }
    }

    pub(crate) fn supported(self, primitive: Primitive) -> bool {
        match self {
            Self::Size => primitive != Primitive::Select,
            Self::Opacity => primitive.supports_opacity(),
            Self::Roundness => primitive.supports_roundness(),
        }
    }

    pub(crate) fn bounds(self, size_range: SizeRange) -> (f32, f32) {
        match self {
            Self::Size => (size_range.min, size_range.max),
            Self::Opacity => (MIN_OPACITY, 1.0),
            Self::Roundness => (0.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PerProperty<T> {
    size: T,
    opacity: T,
    roundness: T,
}

impl<T> PerProperty<T> {
    fn try_new(mut resolve: impl FnMut(Property) -> Result<T, String>) -> Result<Self, String> {
        Ok(Self {
            size: resolve(Property::Size)?,
            opacity: resolve(Property::Opacity)?,
            roundness: resolve(Property::Roundness)?,
        })
    }

    pub(crate) fn get(&self, property: Property) -> &T {
        match property {
            Property::Size => &self.size,
            Property::Opacity => &self.opacity,
            Property::Roundness => &self.roundness,
        }
    }
}

fn valid_step(name: &str, value: f32) -> Result<f32, String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{name} must be greater than 0"));
    }
    Ok(value)
}

fn valid_stops(name: &str, values: &[f32], (min, max): (f32, f32)) -> Result<Arc<[f32]>, String> {
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
            .resolve(SizeRange::DEFAULT)
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
        let feedback_duration = duration(
            "feedback_duration_ms",
            file.feedback_duration_ms.unwrap_or(500),
            0,
        )?;
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
        let global_stops = PerProperty::try_new(|property| {
            let values = file.stops.get(property).as_deref().unwrap_or_default();
            let name = format!("stops.{}", property.name());
            valid_stops(&name, values, property.bounds(global_range))
        })?;
        for property in Property::ALL {
            if let Some(step) = file.steps.get(property) {
                valid_step(&format!("steps.{}", property.name()), *step)?;
            }
        }
        let mut tools = Vec::with_capacity(configs.len());
        for (id, config) in configs {
            crate::cli::tool_name(&id).map_err(|error| format!("tools.{id}: {error}"))?;
            let prefix = format!("tools.{id}");
            let primitive = config.primitive;
            let built_in = id == primitive.name();
            let built_in_text = built_in && primitive == Primitive::Text;
            let unsupported = |field: &str| {
                let tool = primitive.name();
                Err(format!("{prefix}.{field} is not supported by {tool}"))
            };
            for property in Property::ALL
                .into_iter()
                .filter(|p| !p.supported(primitive))
            {
                let name = property.name();
                if config.value(property).is_some() {
                    return unsupported(name);
                } else if property == Property::Size && config.size_range.is_some() {
                    return unsupported("size_range");
                } else if config.steps.get(property).is_some() {
                    return unsupported(&format!("steps.{name}"));
                } else if config.stops.get(property).is_some() {
                    return unsupported(&format!("stops.{name}"));
                }
            }
            for (field, configured, supported) in [
                ("filled", config.filled.is_some(), primitive.supports_fill()),
                (
                    "background",
                    config.background.is_some(),
                    primitive == Primitive::Text,
                ),
                ("font", config.font.is_some(), primitive == Primitive::Text),
            ] {
                if configured && !supported {
                    return unsupported(field);
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
                file.size_range.resolve(SizeRange::TEXT)
            } else {
                global_range
            };
            let range = config
                .size_range
                .as_ref()
                .map_or(fallback_range, |value| value.resolve(fallback_range))
                .validate(&format!("{prefix}.size_range"))?;
            let values = PerProperty::try_new(|property| {
                let value = config.value(property).unwrap_or_else(|| match property {
                    Property::Size => range.clamp(match (built_in, primitive) {
                        (true, Primitive::Text) => 16.0,
                        (true, Primitive::Eraser) => 10.0,
                        _ => stroke_size,
                    }),
                    Property::Opacity => default_color[3],
                    Property::Roundness => match primitive {
                        Primitive::Pen => 1.0,
                        Primitive::Line => 0.5,
                        Primitive::Arrow => 0.25,
                        Primitive::Rectangle | Primitive::Text => 0.1,
                        _ => 0.0,
                    },
                });
                let (min, max) = property.bounds(range);
                if value.is_finite() && (min..=max).contains(&value) {
                    Ok(value)
                } else if property == Property::Size {
                    Err(format!("{prefix}.size must be within its size_range"))
                } else {
                    Err(format!(
                        "{prefix}.{} must be between {min} and {max}",
                        property.name()
                    ))
                }
            })?;
            let steps = PerProperty::try_new(|property| {
                let default = match property {
                    Property::Size if built_in_text => DEFAULT_TEXT_SIZE_STEP,
                    Property::Size => 1.0,
                    Property::Opacity | Property::Roundness => 0.01,
                };
                let step = config.steps.get(property).or(*file.steps.get(property));
                let name = format!("{prefix}.steps.{}", property.name());
                valid_step(&name, step.unwrap_or(default))
            })?;
            let stops = PerProperty::try_new(|property| {
                let bounds = property.bounds(range);
                match config.stops.get(property) {
                    Some(values) => valid_stops(
                        &format!("{prefix}.stops.{}", property.name()),
                        values,
                        bounds,
                    ),
                    // Inherited size stops outside this tool's range are dropped.
                    None => Ok(global_stops
                        .get(property)
                        .iter()
                        .copied()
                        .filter(|value| (bounds.0..=bounds.1).contains(value))
                        .collect()),
                }
            })?;
            let font = (primitive == Primitive::Text)
                .then(|| {
                    config
                        .font
                        .unwrap_or_default()
                        .resolve(&prefix, built_in)
                        .map(Arc::new)
                })
                .transpose()?;
            let icon = match config.icon {
                Some(icon) => icon
                    .resolve(&file.directory)
                    .map_err(|error| format!("{prefix}.icon: {error}"))?,
                None => ToolIcon::Named(primitive.into()),
            };
            tools.push(ToolDefinition {
                id: id.into(),
                primitive,
                icon,
                properties: ToolProperties {
                    size: values.size,
                    opacity: values.opacity,
                    roundness: values.roundness,
                    filled: if primitive == Primitive::Text {
                        config.background.unwrap_or(false)
                    } else {
                        config
                            .filled
                            .unwrap_or(file.default_fill_shapes && primitive.supports_fill())
                    },
                },
                size_range: range,
                steps,
                stops,
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
            draw_on: file.draw_on,
            tools: tools.into(),
            default_tool,
            tool_wheel,
            remember_last_tool: file.remember_last_tool.unwrap_or(true),
            remember_tool_properties: file.remember_tool_properties.unwrap_or(true),
            default_color,
            palette,
            feedback_duration,
            presentation: file.presentation.resolve()?,
            annotation_shadows: file.annotation_shadows.unwrap_or(true),
            annotation_shadow_overlap: file.annotation_shadow_overlap,
            clear_on_cancel: file.clear_on_cancel,
            freeze_on_activate: file.freeze_on_activate,
            annotations_visible: file.annotations_visible.unwrap_or(true),
            whiteboard_color: crate::color_to_srgb(
                crate::cli::opaque_color(file.whiteboard.color.as_deref().unwrap_or("#EBEBEB"))
                    .map_err(|error| format!("whiteboard.color: {error}"))?,
            ),
        })
    }
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
