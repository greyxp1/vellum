#[derive(Clone, Copy, Debug)]
pub(crate) struct SizeRange {
    pub(crate) min: f32,
    pub(crate) max: f32,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SizeRangeConfig {
    min: Option<f32>,
    max: Option<f32>,
}

impl SizeRangeConfig {
    pub(super) fn resolve(&self, fallback: SizeRange) -> SizeRange {
        SizeRange {
            min: self.min.unwrap_or(fallback.min),
            max: self.max.unwrap_or(fallback.max),
        }
    }
}

impl SizeRange {
    pub(super) const DEFAULT: Self = Self {
        min: 1.0,
        max: 100.0,
    };
    pub(super) const TEXT: Self = Self {
        min: 8.0,
        max: 500.0,
    };

    pub(super) fn validate(self, name: &str) -> Result<Self, String> {
        if !self.min.is_finite() || self.min <= 0.0 {
            return Err(format!("{name}.min must be greater than 0"));
        }
        if !self.max.is_finite() || self.max < self.min {
            return Err(format!(
                "{name}.max must be greater than or equal to {name}.min"
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
}
