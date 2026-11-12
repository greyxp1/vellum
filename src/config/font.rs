use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FontConfig {
    family: Option<String>,
    weight: Option<f32>,
    style: Option<String>,
    features: Option<BTreeMap<String, u16>>,
}

impl FontConfig {
    pub(super) fn resolve(
        &self,
        tool: &str,
        built_in: bool,
    ) -> Result<crate::text::TextFont, String> {
        use parley::setting::Tag;
        use parley::style::{FontFamilyName, FontFeature, FontStyle, FontWeight};

        let prefix = format!("{tool}.font");
        if !built_in {
            for (field, missing) in [
                ("family", self.family.is_none()),
                ("weight", self.weight.is_none()),
                ("style", self.style.is_none()),
                ("features", self.features.is_none()),
            ] {
                if missing {
                    return Err(format!("{prefix}.{field} is required for custom tools"));
                }
            }
        }
        let family = FontFamilyName::parse_css_list(self.family.as_deref().unwrap_or("sans-serif"))
            .map(|name| name.map(FontFamilyName::into_owned))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("invalid {prefix}.family: {error}"))?;
        if family.is_empty() {
            return Err(format!("{prefix}.family must not be empty"));
        }
        let weight = self.weight.unwrap_or(400.0);
        if !weight.is_finite() || !(1.0..=1000.0).contains(&weight) {
            return Err(format!("{prefix}.weight must be between 1 and 1000"));
        }
        let style = match self.style.as_deref().unwrap_or("normal") {
            "normal" => FontStyle::Normal,
            "italic" => FontStyle::Italic,
            "oblique" => FontStyle::Oblique(Some(14.0)),
            _ => return Err(format!("{prefix}.style must be normal, italic, or oblique")),
        };
        let features = self
            .features
            .iter()
            .flatten()
            .map(|(name, &value)| {
                let tag = Tag::parse(name).ok_or_else(|| {
                    format!(
                        "invalid {prefix}.features tag {name:?}: expected four printable ASCII characters"
                    )
                })?;
                Ok(FontFeature::new(tag, value))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(crate::text::TextFont {
            family,
            weight: FontWeight::new(weight),
            style,
            features,
        })
    }
}
