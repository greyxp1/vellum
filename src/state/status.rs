use std::borrow::Cow;
use std::fmt::Write;

use clap::ValueEnum;
use serde::{Serialize, Serializer};

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

    fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Mode => "Mode",
            Self::Whiteboard => "Whiteboard",
            Self::Presentation => "Presentation",
            Self::Tool => "Tool",
            Self::Color => "Drawing color",
            Self::Size => "Tool size",
            Self::Opacity => "Tool opacity",
            Self::Roundness => "Tool roundness",
            Self::Filled => "Tool fill/background",
            Self::TextEditing => "Text editing",
            Self::AnnotationsVisible => "Annotations outside drawing mode",
            Self::WhiteboardColor => "Whiteboard color",
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
            Self::Boolean(value) => value.to_string().into(),
            Self::Text(text) => text.into(),
            Self::Number(number) => number.to_string().into(),
            Self::Color([r, g, b, a]) => format!("color(srgb {r} {g} {b} / {a})").into(),
            Self::Null => "null".into(),
        }
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
        let value = |field| self.status_value(field, tool, style);
        let fields = StatusField::value_variants()
            .iter()
            .copied()
            .filter(|field| tool.is_none() || field.is_tool_field());
        if json {
            let mut output = Vec::new();
            let mut serializer = serde_json::Serializer::new(&mut output);
            match field {
                Some(field) => value(field).serialize(&mut serializer),
                None => serializer.collect_map(fields.map(|field| {
                    let name = field.to_possible_value().unwrap().get_name().to_owned();
                    (name.replace('-', "_"), value(field))
                })),
            }
            .map_err(|error| error.to_string())?;
            return Ok(String::from_utf8(output).unwrap().into());
        }
        if let Some(field) = field {
            return Ok(value(field).plain());
        }
        let mut output = String::new();
        for field in fields {
            let value = match value(field) {
                _ if matches!(field, StatusField::Active | StatusField::Whiteboard) => continue,
                Value::Null => continue,
                Value::Number(number) if field == StatusField::Size => {
                    format!("{number} px").into()
                }
                Value::Number(number) => format!("{:.0}%", number * 100.0).into(),
                Value::Boolean(value) => if value { "yes" } else { "no" }.into(),
                value => value.plain(),
            };
            writeln!(output, "{}: {value}", field.label()).unwrap();
        }
        Ok(output.into())
    }
}
