use std::borrow::Cow;
use std::fmt::Write;

use clap::ValueEnum;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use super::{Mode, State};
use crate::cli::StatusField;
use crate::tool::Tool;

impl StatusField {
    pub(crate) fn is_tool_field(self) -> bool {
        matches!(
            self,
            Self::Tool | Self::Color | Self::Size | Self::Opacity | Self::Roundness | Self::Filled
        )
    }

    pub(crate) fn is_boolean(self) -> bool {
        matches!(
            self,
            Self::Active
                | Self::Whiteboard
                | Self::TextEditing
                | Self::AnnotationsVisible
                | Self::Filled
        )
    }

    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Active => ("active", "Active"),
            Self::Mode => ("mode", "Mode"),
            Self::Whiteboard => ("whiteboard", "Whiteboard"),
            Self::Tool => ("tool", "Tool"),
            Self::Color => ("color", "Tool color"),
            Self::Size => ("size", "Tool size"),
            Self::Opacity => ("opacity", "Tool opacity"),
            Self::Roundness => ("roundness", "Tool roundness"),
            Self::Filled => ("filled", "Tool fill/background"),
            Self::TextEditing => ("text_editing", "Text editing"),
            Self::AnnotationsVisible => ("annotations_visible", "Annotations outside drawing mode"),
            Self::WhiteboardColor => ("whiteboard_color", "Whiteboard color"),
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Value {
    Boolean(bool),
    Text(&'static str),
    Number(f32),
    Color(crate::Rgba),
    Null,
}

impl Value {
    fn plain(self) -> Cow<'static, str> {
        match self {
            Self::Boolean(true) => "true".into(),
            Self::Boolean(false) => "false".into(),
            Self::Text(text) => text.into(),
            Self::Number(number) => number.to_string().into(),
            Self::Color([r, g, b, a]) => format!("color(srgb {r} {g} {b} / {a})").into(),
            Self::Null => "null".into(),
        }
    }
}

fn fields(tool: Option<Tool>) -> impl Iterator<Item = StatusField> {
    StatusField::value_variants()
        .iter()
        .copied()
        .filter(move |field| tool.is_none() || field.is_tool_field())
}

struct Snapshot<'a>(&'a State, Option<Tool>);

impl Serialize for Snapshot<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for field in fields(self.1) {
            map.serialize_entry(field.names().0, &self.0.status_value(field, self.1))?;
        }
        map.end()
    }
}

impl State {
    fn status_value(&self, field: StatusField, target: Option<Tool>) -> Value {
        let (tool, settings) = self.draw.tool_style(target);
        match field {
            StatusField::Active => Value::Boolean(self.is_active()),
            StatusField::Mode => Value::Text(match self.mode {
                Mode::Inactive => "inactive",
                Mode::Desktop => "desktop",
                Mode::Whiteboard => "whiteboard",
            }),
            StatusField::Whiteboard => Value::Boolean(self.is_whiteboard()),
            StatusField::TextEditing => Value::Boolean(self.is_text_editing()),
            StatusField::AnnotationsVisible => Value::Boolean(self.annotations_visible),
            StatusField::WhiteboardColor => Value::Color(self.whiteboard_color),
            StatusField::Tool => Value::Text(tool.name()),
            StatusField::Color if tool.supports_opacity() => Value::Color(settings.color),
            StatusField::Size if tool != Tool::Select => Value::Number(settings.size),
            StatusField::Opacity if tool.supports_opacity() => Value::Number(settings.color[3]),
            StatusField::Roundness if tool.supports_roundness() => {
                Value::Number(settings.roundness)
            }
            StatusField::Filled if tool.supports_fill() || tool == Tool::Text => {
                Value::Boolean(settings.filled)
            }
            StatusField::Color
            | StatusField::Size
            | StatusField::Opacity
            | StatusField::Roundness
            | StatusField::Filled => Value::Null,
        }
    }

    pub(crate) fn status_reply(
        &self,
        field: Option<StatusField>,
        tool: Option<Tool>,
        json: bool,
    ) -> Result<Cow<'static, str>, serde_json::Error> {
        if let Some(field) = field {
            let value = self.status_value(field, tool);
            return if json && !matches!(value, Value::Boolean(_) | Value::Null) {
                serde_json::to_string(&value).map(Cow::Owned)
            } else {
                Ok(value.plain())
            };
        }
        if json {
            return serde_json::to_string(&Snapshot(self, tool)).map(Cow::Owned);
        }
        let mut output = String::new();
        for field in fields(tool) {
            let value = self.status_value(field, tool);
            if matches!(field, StatusField::Active | StatusField::Whiteboard)
                || matches!(value, Value::Null)
            {
                continue;
            }
            let label = field.names().1;
            match value {
                Value::Number(number) if matches!(field, StatusField::Size) => {
                    writeln!(output, "{label}: {number} px")
                }
                Value::Number(number)
                    if matches!(field, StatusField::Roundness | StatusField::Opacity) =>
                {
                    writeln!(output, "{label}: {:.0}%", number * 100.0)
                }
                Value::Boolean(value) => {
                    writeln!(output, "{label}: {}", if value { "yes" } else { "no" })
                }
                value => writeln!(output, "{label}: {}", value.plain()),
            }
            .expect("writing to a String cannot fail");
        }
        Ok(output.into())
    }
}
