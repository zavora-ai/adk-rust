use serde_json::{Value, json};

use super::spec::KitSpec;

#[derive(Debug, Clone)]
pub struct KitArtifacts {
    pub catalog: Value,
    pub tokens: Value,
    pub templates: Value,
    pub theme_css: String,
}

#[derive(Debug, Default)]
pub struct KitGenerator;

impl KitGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate(&self, spec: &KitSpec) -> KitArtifacts {
        let catalog_id = format!("zavora.ai:adk-ui/kit/{}@{}", slugify(&spec.name), spec.version);

        let catalog = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": catalog_id,
            "title": format!("ADK-UI Kit: {}", spec.name),
            "description": format!("Generated UI kit for {}", spec.name),
            "catalogId": catalog_id,
            "components": {},
            "theme": {
                "primaryColor": spec.colors.primary,
                "agentDisplayName": spec.name
            }
        });

        let tokens = json!({
            "colors": {
                "primary": spec.colors.primary,
                "accent": spec.colors.accent,
                "surface": spec.colors.surface,
                "background": spec.colors.background,
                "text": spec.colors.text
            },
            "typography": {
                "family": spec.typography.family,
                "scale": spec.typography.scale
            },
            "density": format!("{:?}", spec.density).to_lowercase(),
            "radius": format!("{:?}", spec.radius).to_lowercase()
        });

        let templates = json!({
            "templates": spec.templates
        });

        let theme_css = format!(
            ":root {{\n  --adk-primary: {};\n  --adk-font-family: \"{}\";\n}}\n",
            spec.colors.primary, spec.typography.family
        );

        KitArtifacts { catalog, tokens, templates, theme_css }
    }
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            if !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::spec::{KitBrand, KitColors, KitSpec, KitTypography};

    #[test]
    fn generates_catalog_with_expected_id() {
        let spec = KitSpec {
            name: "Fintech Pro".to_string(),
            version: "0.1.0".to_string(),
            brand: KitBrand { vibe: "trustworthy".to_string(), industry: None },
            colors: KitColors {
                primary: "#2F6BFF".to_string(),
                accent: None,
                surface: None,
                background: None,
                text: None,
            },
            typography: KitTypography { family: "Source Sans 3".to_string(), scale: None },
            density: Default::default(),
            radius: Default::default(),
            components: Default::default(),
            templates: vec!["auth_login".to_string()],
        };

        let artifacts = KitGenerator::new().generate(&spec);
        assert_eq!(artifacts.catalog["catalogId"], "zavora.ai:adk-ui/kit/fintech-pro@0.1.0");
        assert!(artifacts.tokens["colors"]["primary"].is_string());
    }
}
