//! Studio colour themes as data: the `theme` artifact kind.
//!
//! A theme is Studio's `StudioTheme` JSON (`apps/studio/src/lib/studio-theme.ts`) carried
//! as a definition, so whoever may apply definitions to a workspace (one org and
//! environment) may restyle it, and an agent changes the look by applying a definition, with
//! no redeploy. A theme only recolours Studio's allowlisted semantic tokens over one of its
//! built-in skins; it never carries layout, geometry or CSS.
//!
//! Which tokens and skins exist, and the contrast and monochrome rules a theme must meet,
//! are admission rules in `norn-ui-checker` (`theme.*` diagnostics), measured against the
//! palettes Studio renders. This module only fixes the wire shape.

use crate::{JsonValue, PayloadField};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The literal `1`: the only `StudioTheme` version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ThemeVersion1;

impl Serialize for ThemeVersion1 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}

impl<'de> Deserialize<'de> for ThemeVersion1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if u64::deserialize(deserializer)? == 1 {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "only theme version 1 is supported",
            ))
        }
    }
}

impl JsonSchema for ThemeVersion1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ThemeVersion1".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "const": 1 })
    }
}

/// The colour scheme a theme is limited to; absent means it applies to both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAppearance {
    Light,
    Dark,
}

/// Semantic token name (without the leading `--`) to a CSS colour.
pub type ThemeTokenMap = BTreeMap<String, String>;

/// Per-scheme token overrides, applied over [`Theme::colors`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThemeVariants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<ThemeTokenMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<ThemeTokenMap>,
}

/// The strict typed root of a `theme` artifact payload: Studio's `StudioTheme`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Theme {
    pub version: ThemeVersion1,
    /// Display name, 1 to 64 characters after trimming.
    pub name: String,
    /// The built-in Studio skin the theme recolours (`luma` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_skin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<ThemeAppearance>,
    /// Shorthand for the action, primary, ring and link tokens, with a contrasting ink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// Shorthand for the canvas and shell tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<ThemeTokenMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<ThemeVariants>,
}

define_payload_root!(
    /// The payload root of a `theme` artifact: [`Theme`] on the wire.
    #[schemars(with = "Theme")]
    ThemePayload {
        required {
            version => "version" as ThemeVersion1,
            name => "name" as String
        }
        optional {
            base_skin => "baseSkin" as String,
            appearance => "appearance" as ThemeAppearance,
            accent => "accent" as String,
            canvas => "canvas" as String,
            colors => "colors" as ThemeTokenMap,
            variants => "variants" as ThemeVariants
        }
    }
);
