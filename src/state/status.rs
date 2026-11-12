use std::borrow::Cow;
use std::fmt::Write;

use clap::ValueEnum;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use super::{Mode, State};
use crate::cli::StatusField;
use crate::draw::Style;
use crate::tool::Primitive;

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
                | Self::Presentation
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
            Self::Presentation => ("presentation", "Presentation"),
            Self::Tool => ("tool", "Tool"),
            Self::Color => ("color", "Drawing color"),
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
    Text(String),
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

fn fields(tool: Option<&str>) -> impl Iterator<Item = StatusField> + '_ {
    StatusField::value_variants()
        .iter()
        .copied()
        .filter(move |field| tool.is_none() || field.is_tool_field())
}

struct Snapshot<'a> {
    state: &'a State,
    target: Option<&'a str>,
    style: (Primitive, Style),
}

impl Serialize for Snapshot<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for field in fields(self.target) {
            let value = self.state.status_value(field, self.target, self.style);
            map.serialize_entry(field.names().0, &value)?;
        }
        map.end()
    }
}

impl State {
    fn status_value(
        &self,
        field: StatusField,
        target: Option<&str>,
        (tool, settings): (Primitive, Style),
    ) -> Value {
        match field {
            StatusField::Active => Value::Boolean(self.is_active()),
            StatusField::Mode => Value::Text(
                match self.mode {
                    Mode::Inactive => "inactive",
                    Mode::Desktop => "desktop",
                    Mode::Whiteboard => "whiteboard",
                }
                .into(),
            ),
            StatusField::Whiteboard => Value::Boolean(self.is_whiteboard()),
            StatusField::Presentation => Value::Boolean(self.draw.is_presenting()),
            StatusField::TextEditing => Value::Boolean(self.is_text_editing()),
            StatusField::AnnotationsVisible => Value::Boolean(self.annotations_visible),
            StatusField::WhiteboardColor => Value::Color(self.whiteboard_color),
            StatusField::Tool => Value::Text(target.unwrap_or(self.draw.tool_name()).into()),
            StatusField::Color => {
                let [r, g, b, alpha] = settings.color;
                Value::Color([r, g, b, if tool.supports_opacity() { alpha } else { 1.0 }])
            }
            StatusField::Size if tool != Primitive::Select => Value::Number(settings.size),
            StatusField::Opacity if tool.supports_opacity() => Value::Number(settings.color[3]),
            StatusField::Roundness if tool.supports_roundness() => {
                Value::Number(settings.roundness)
            }
            StatusField::Filled if tool.supports_fill() || tool == Primitive::Text => {
                Value::Boolean(settings.filled)
            }
            StatusField::Size
            | StatusField::Opacity
            | StatusField::Roundness
            | StatusField::Filled => Value::Null,
        }
    }

    pub(crate) fn status_reply(
        &self,
        field: Option<StatusField>,
        tool: Option<String>,
        json: bool,
    ) -> Result<Cow<'static, str>, String> {
        let tool = tool.as_deref();
        let style = self.draw.named_tool_style(tool)?;
        if let Some(field) = field {
            let value = self.status_value(field, tool, style);
            return if json && !matches!(value, Value::Boolean(_) | Value::Null) {
                serde_json::to_string(&value)
                    .map(Cow::Owned)
                    .map_err(|error| error.to_string())
            } else {
                Ok(value.plain())
            };
        }
        if json {
            return serde_json::to_string(&Snapshot {
                state: self,
                target: tool,
                style,
            })
            .map(Cow::Owned)
            .map_err(|error| error.to_string());
        }
        let mut output = String::new();
        for field in fields(tool) {
            let value = self.status_value(field, tool, style);
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
