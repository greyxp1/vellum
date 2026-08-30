use super::cli::Cli;
use super::{Rgb, state};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CONFIG_FILE: &str = "vellum/config.toml";
const DEFAULT_PALETTE: [&str; 8] = [
    "#E84046", "#EC8948", "#EED049", "#3ED73C", "#0283FC", "#7C57EB", "#FFFFFF", "#000000",
];

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    default_tool: Option<String>,
    remember_last_tool: Option<bool>,
    stroke_size: Option<f32>,
    #[serde(default)]
    size_range: SizeRangeConfig,
    default_color: Option<String>,
    palette: Option<Vec<String>>,
    feedback_duration_ms: Option<u64>,
    clear_on_escape: Option<bool>,
    default_fill_shapes: Option<bool>,
    #[serde(default)]
    tools: ToolDefaults,
}

pub(crate) type ToolDefaults = BTreeMap<state::Tool, PropertyDefaults>;

#[derive(Clone, Debug)]
pub(crate) struct SizeRange {
    min: f32,
    max: f32,
    step: f32,
    stops: Arc<[f32]>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SizeRangeConfig {
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
    stops: Option<Vec<f32>>,
}

impl SizeRangeConfig {
    fn resolve(&self, fallback: &SizeRange) -> SizeRange {
        let stops = self.stops.as_ref().map_or_else(
            || fallback.stops.clone(),
            |stops| {
                let mut stops = stops.clone();
                stops.sort_by(f32::total_cmp);
                stops.dedup();
                Arc::from(stops)
            },
        );
        SizeRange {
            min: self.min.unwrap_or(fallback.min),
            max: self.max.unwrap_or(fallback.max),
            step: self.step.unwrap_or(fallback.step),
            stops,
        }
    }
}

impl Default for SizeRange {
    fn default() -> Self {
        Self {
            min: 1.0,
            max: 100.0,
            step: 1.0,
            stops: Arc::from([]),
        }
    }
}

impl SizeRange {
    fn validate(self, name: &str) -> Result<Self, String> {
        if !self.min.is_finite() || self.min <= 0.0 {
            return Err(format!("{name}.min must be greater than 0"));
        }
        if !self.max.is_finite() || self.max < self.min {
            return Err(format!(
                "{name}.max must be greater than or equal to {name}.min"
            ));
        }
        if !self.step.is_finite() || self.step <= 0.0 {
            return Err(format!("{name}.step must be greater than 0"));
        }
        if self.stops.iter().any(|stop| !self.contains(*stop)) {
            return Err(format!(
                "{name}.stops values must be between {} and {}",
                self.min, self.max,
            ));
        }
        Ok(self)
    }

    pub(crate) fn contains(&self, size: f32) -> bool {
        size.is_finite() && (self.min..=self.max).contains(&size)
    }

    pub(crate) fn clamp(&self, size: f32) -> f32 {
        size.clamp(self.min, self.max)
    }

    pub(crate) fn min(&self) -> f32 {
        self.min
    }

    pub(crate) fn max(&self) -> f32 {
        self.max
    }

    pub(crate) fn step(&self) -> f32 {
        self.step
    }

    pub(crate) fn stops(&self) -> &[f32] {
        &self.stops
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PropertyDefaults {
    pub(crate) size: Option<f32>,
    pub(crate) size_range: Option<SizeRangeConfig>,
    pub(crate) opacity: Option<f32>,
    pub(crate) roundness: Option<f32>,
    pub(crate) filled: Option<bool>,
}

fn resolve_size_ranges(
    tools: &ToolDefaults,
    fallback: &SizeRange,
) -> Result<BTreeMap<state::Tool, SizeRange>, String> {
    state::Tool::SIZED
        .into_iter()
        .map(|tool| {
            let name = format!("tools.{}.size_range", tool.name());
            let range = tools
                .get(&tool)
                .and_then(|defaults| defaults.size_range.as_ref())
                .map_or_else(|| fallback.clone(), |range| range.resolve(fallback))
                .validate(&name)?;
            Ok((tool, range))
        })
        .collect()
}

fn validate_tool_defaults(
    tools: &ToolDefaults,
    size_ranges: &BTreeMap<state::Tool, SizeRange>,
) -> Result<(), String> {
    for (&tool, defaults) in tools {
        let prefix = format!("tools.{}", tool.name());
        let supports_size = tool != state::Tool::Select;
        if defaults.size_range.is_some() && !supports_size {
            return Err(format!("{prefix}.size_range is not supported"));
        }
        match defaults.size {
            Some(_) if !supports_size => {
                return Err(format!("{prefix}.size is not supported"));
            }
            Some(size)
                if !size_ranges
                    .get(&tool)
                    .is_some_and(|range| range.contains(size)) =>
            {
                let size_range = size_ranges
                    .get(&tool)
                    .expect("tools with size defaults have size ranges");
                return Err(format!(
                    "{prefix}.size must be between {} and {}",
                    size_range.min(),
                    size_range.max(),
                ));
            }
            _ => {}
        }
        match defaults.opacity {
            Some(_) if matches!(tool, state::Tool::Eraser | state::Tool::Select) => {
                return Err(format!("{prefix}.opacity is not supported"));
            }
            Some(value) if !value.is_finite() || !(0.05..=1.0).contains(&value) => {
                return Err(format!("{prefix}.opacity must be between 0.05 and 1.0"));
            }
            _ => {}
        }
        if defaults.roundness.is_some() && tool.default_roundness().is_none() {
            return Err(format!("{prefix}.roundness is not supported"));
        }
        if let Some(value) = defaults.roundness
            && (!value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(format!("{prefix}.roundness must be between 0.0 and 1.0"));
        }
        if defaults.filled.is_some() && !tool.supports_fill() {
            return Err(format!("{prefix}.filled is not supported"));
        }
    }
    Ok(())
}

pub(super) struct Settings {
    pub(super) stroke_size: f32,
    pub(super) size_ranges: Arc<BTreeMap<state::Tool, SizeRange>>,
    pub(super) default_color: Rgb,
    pub(super) default_tool: state::Tool,
    pub(super) remember_last_tool: bool,
    pub(super) palette: Vec<Rgb>,
    pub(super) feedback_duration: Duration,
    pub(super) clear_on_escape: bool,
    pub(super) default_fill_shapes: bool,
    pub(super) tool_defaults: ToolDefaults,
}

impl Settings {
    pub(super) fn load(cli: Cli) -> Result<Self, String> {
        let file = if cli.no_config {
            FileConfig::default()
        } else if let Some(path) = &cli.config {
            read_config(path)?
        } else {
            read_first_config(default_config_paths())?
        };

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
            .map(|(index, color)| parse_named_color(&format!("palette[{index}]"), color))
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

        let size_ranges = Arc::new(resolve_size_ranges(&file.tools, &size_range)?);
        validate_tool_defaults(&file.tools, &size_ranges)?;

        Ok(Self {
            stroke_size,
            size_ranges,
            default_color,
            default_tool,
            remember_last_tool: file.remember_last_tool.unwrap_or(true),
            palette,
            feedback_duration: Duration::from_millis(feedback_duration_ms),
            clear_on_escape: file.clear_on_escape.unwrap_or(false),
            default_fill_shapes: file.default_fill_shapes.unwrap_or(false),
            tool_defaults: file.tools,
        })
    }
}

fn parse_named_color(name: &str, value: &str) -> Result<Rgb, String> {
    parse_color(value).map_err(|error| format!("invalid {name} {value:?}: {error}"))
}

fn parse_color(value: &str) -> Result<Rgb, &'static str> {
    let hex = value.strip_prefix('#').ok_or("color must start with #")?;
    if hex.len() != 6 {
        return Err("color must be #RRGGBB");
    }
    let value = u32::from_str_radix(hex, 16).map_err(|_| "color contains a non-hex digit")?;
    Ok([
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        (value & 0xff) as f32 / 255.0,
    ])
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
    config_paths(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("XDG_CONFIG_DIRS"),
    )
}

fn config_paths(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
    xdg_config_dirs: Option<OsString>,
) -> Vec<PathBuf> {
    let user = absolute_path(xdg_config_home)
        .or_else(|| absolute_path(home).map(|path| path.join(".config")));
    let mut paths: Vec<_> = user
        .into_iter()
        .map(|path| path.join(CONFIG_FILE))
        .collect();

    match xdg_config_dirs.filter(|value| !value.is_empty()) {
        Some(dirs) => paths.extend(
            std::env::split_paths(&dirs)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(CONFIG_FILE)),
        ),
        None => paths.push(PathBuf::from("/etc/xdg").join(CONFIG_FILE)),
    }
    paths
}

fn absolute_path(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}
