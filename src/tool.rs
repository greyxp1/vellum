#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Primitive {
    #[default]
    Pen,
    Line,
    Arrow,
    Triangle,
    Rectangle,
    Ellipse,
    Text,
    Eraser,
    Select,
}

impl Primitive {
    pub(crate) const ALL: [Self; 9] = [
        Self::Select,
        Self::Pen,
        Self::Line,
        Self::Arrow,
        Self::Triangle,
        Self::Rectangle,
        Self::Ellipse,
        Self::Text,
        Self::Eraser,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Pen => "pen",
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::Triangle => "triangle",
            Self::Rectangle => "rectangle",
            Self::Ellipse => "ellipse",
            Self::Text => "text",
            Self::Eraser => "eraser",
            Self::Select => "select",
        }
    }

    pub(crate) fn supports_fill(self) -> bool {
        matches!(self, Self::Triangle | Self::Rectangle | Self::Ellipse)
    }

    pub(crate) fn supports_opacity(self) -> bool {
        !matches!(self, Self::Eraser | Self::Select)
    }

    pub(crate) fn supports_roundness(self) -> bool {
        matches!(
            self,
            Self::Pen | Self::Line | Self::Arrow | Self::Triangle | Self::Rectangle | Self::Text
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolId(pub usize);

pub(crate) const ICON_COLOR: [f32; 4] = [0.96, 0.97, 1.0, 1.0];

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Icon {
    Pen,
    Marker,
    Line,
    Arrow,
    Triangle,
    Rectangle,
    Ellipse,
    Text,
    Eraser,
    Select,
}

impl From<Primitive> for Icon {
    fn from(primitive: Primitive) -> Self {
        match primitive {
            Primitive::Pen => Self::Pen,
            Primitive::Line => Self::Line,
            Primitive::Arrow => Self::Arrow,
            Primitive::Triangle => Self::Triangle,
            Primitive::Rectangle => Self::Rectangle,
            Primitive::Ellipse => Self::Ellipse,
            Primitive::Text => Self::Text,
            Primitive::Eraser => Self::Eraser,
            Primitive::Select => Self::Select,
        }
    }
}
