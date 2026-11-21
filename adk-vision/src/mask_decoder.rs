//! Which decoder turns `sam-3.1` mask payloads into rasters, as configuration that parses in
//! every build, so `.adk/vision.toml` reads the same with or without the `sam` feature.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The built-in manifest entry whose licence, the SAM License, covers the ported codec
/// ([`MaskDecoderChoice::Licensed`]). `adk-rust vision models accept sam-mask-codec`
/// records a person's acceptance of it.
pub const SAM_MASK_CODEC_MANIFEST: &str = "sam-mask-codec";

/// How long an external decoder may take for one answer's masks, unless configured.
pub const DEFAULT_MASK_DECODER_TIMEOUT_SECS: u64 = 60;

fn default_timeout_secs() -> u64 {
    DEFAULT_MASK_DECODER_TIMEOUT_SECS
}

/// Which decoder a `SamSegmenter` uses for `sam-3.1` masks.
///
/// Meta's mask payloads are base85 text around an arithmetic-coded body whose model only
/// Meta's `@meta-sam/parser` defines, under the SAM License. The Apache-2.0 crates hold no
/// SAM-licensed code, so masks come through one of three choices:
///
/// | Choice | Each object is | Needs |
/// |--------|----------------|-------|
/// | `None` (`boxes`, the default) | Its box in source pixels | Nothing |
/// | `External` | Its mask, decoded by a program you installed | The program; it reads mask JSON on standard input and writes base64 rasters |
/// | `Licensed` | Its mask, decoded in this process by the port of Meta's codec | Feature `sam-codec` and a person's recorded acceptance of the SAM License |
///
/// In `.adk/vision.toml` it is the `[sam.masks]` table, keyed by `decoder`.
///
/// # Example
///
/// ```
/// use adk_vision::{MaskDecoderChoice, VisionConfig};
///
/// let config = VisionConfig::from_toml_str(r#"
///     [sam.masks]
///     decoder = "external"
///     program = "/opt/sam/bin/sam-mask-decode"
///     args = ["--json"]
///     timeout_secs = 30
/// "#).unwrap();
/// assert_eq!(
///     config.sam.masks,
///     MaskDecoderChoice::External {
///         program: "/opt/sam/bin/sam-mask-decode".into(),
///         args: vec!["--json".into()],
///         timeout_secs: 30,
///     }
/// );
/// let licensed = VisionConfig::from_toml_str("[sam.masks]\ndecoder = \"licensed\"").unwrap();
/// assert_eq!(licensed.sam.masks, MaskDecoderChoice::Licensed);
/// assert_eq!(VisionConfig::default().sam.masks, MaskDecoderChoice::None);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decoder", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaskDecoderChoice {
    /// No decoder: each object is its box in source pixels.
    #[default]
    #[serde(rename = "boxes", alias = "none")]
    None,
    /// A program the user installed, such as a wrapper around Meta's parser.
    ///
    /// It is started once per answer with `args`, reads a JSON array of
    /// `{"encoding", "width", "height", "payload"}` on standard input and writes a JSON array
    /// holding one base64 string per mask: `height × width` bytes, row-major, nonzero for a
    /// set pixel. It is ended after `timeout_secs`.
    External {
        /// The program: a path, or a name looked up on `PATH`.
        program: PathBuf,
        /// Arguments passed before anything else.
        #[serde(default)]
        args: Vec<String>,
        /// Seconds one answer's masks may take.
        #[serde(default = "default_timeout_secs")]
        timeout_secs: u64,
    },
    /// The Rust port of Meta's codec (`adk-vision-sam-codec`, under the SAM License), in
    /// this process: feature `sam-codec`, and a person's recorded acceptance of the
    /// [`SAM_MASK_CODEC_MANIFEST`] entry's licence.
    Licensed,
}

impl MaskDecoderChoice {
    /// An external program with arguments and the default timeout.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::MaskDecoderChoice;
    ///
    /// let choice = MaskDecoderChoice::external("sam-mask-decode", ["--json"]);
    /// assert_eq!(choice.name(), "external");
    /// ```
    pub fn external<A: Into<String>>(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = A>,
    ) -> Self {
        Self::External {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout_secs: DEFAULT_MASK_DECODER_TIMEOUT_SECS,
        }
    }

    /// The choice's configuration word: `boxes`, `external` or `licensed`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "boxes",
            Self::External { .. } => "external",
            Self::Licensed => "licensed",
        }
    }
}

/// The `[sam]` table of `.adk/vision.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamSettings {
    /// The `[sam.masks]` table: which decoder turns mask payloads into rasters.
    #[serde(default)]
    pub masks: MaskDecoderChoice,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_and_decoders_are_refused() {
        let parse = |text: &str| toml::from_str::<SamSettings>(text);
        assert_eq!(parse("").unwrap().masks, MaskDecoderChoice::None);
        assert_eq!(parse("[masks]\ndecoder = \"none\"").unwrap().masks, MaskDecoderChoice::None);
        assert!(parse("[masks]\ndecoder = \"codec\"").is_err());
        assert!(parse("[masks]\nprogram = \"x\"").is_err());
        assert!(parse("decoder = \"boxes\"").is_err());
        assert!(parse("[masks]\ndecoder = \"external\"\nprogram = \"d\"\ntimeout = 5").is_err());
        let external = parse("[masks]\ndecoder = \"external\"\nprogram = \"d\"").unwrap();
        assert_eq!(external.masks, MaskDecoderChoice::external("d", Vec::<String>::new()));
    }
}
