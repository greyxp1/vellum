#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
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

impl Tool {
    pub(crate) const SIZED: [Self; 8] = [
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

    pub(crate) fn initial_size(self, stroke_size: f32) -> Option<f32> {
        match self {
            Self::Text => Some(16.0),
            Self::Eraser => Some(10.0),
            Self::Select => None,
            _ => Some(stroke_size),
        }
    }

    pub(crate) fn default_roundness(self) -> Option<f32> {
        match self {
            Self::Pen
            | Self::Line
            | Self::Arrow
            | Self::Triangle
            | Self::Rectangle
            | Self::Text => Some(self.initial_roundness()),
            _ => None,
        }
    }

    pub(super) fn initial_roundness(self) -> f32 {
        match self {
            Self::Pen => 1.0,
            Self::Line => 0.5,
            Self::Arrow => 0.25,
            Self::Rectangle => 0.01,
            Self::Text => 0.1,
            _ => 0.0,
        }
    }
}

impl std::str::FromStr for Tool {
    type Err = &'static str;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "pen" => Ok(Self::Pen),
            "line" => Ok(Self::Line),
            "arrow" => Ok(Self::Arrow),
            "triangle" => Ok(Self::Triangle),
            "rectangle" => Ok(Self::Rectangle),
            "ellipse" => Ok(Self::Ellipse),
            "text" => Ok(Self::Text),
            "eraser" => Ok(Self::Eraser),
            "select" => Ok(Self::Select),
            _ => Err(
                "default tool must be pen, line, arrow, triangle, rectangle, ellipse, text, eraser, or select",
            ),
        }
    }
}
