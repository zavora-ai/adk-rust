use crate::document::SchemaDirection;

mod sealed {
    pub trait Sealed {}
}

/// Marker trait for schema roles, sealed to prevent external implementation.
pub trait SchemaRole: sealed::Sealed + Send + Sync + 'static {
    /// The runtime direction associated with the role.
    const DIRECTION: SchemaDirection;
    /// The tag used during digest calculation.
    const DIGEST_TAG: u8;
}

/// Role for tool inputs.
#[derive(Debug)]
pub enum Input {}

/// Role for tool outputs.
#[derive(Debug)]
pub enum Output {}

impl sealed::Sealed for Input {}
impl sealed::Sealed for Output {}

impl SchemaRole for Input {
    const DIRECTION: SchemaDirection = SchemaDirection::Input;
    const DIGEST_TAG: u8 = 1;
}

impl SchemaRole for Output {
    const DIRECTION: SchemaDirection = SchemaDirection::Output;
    const DIGEST_TAG: u8 = 2;
}

/// Type alias for input schemas.
pub type InputSchema = crate::document::SchemaDocument<Input>;
/// Type alias for output schemas.
pub type OutputSchema = crate::document::SchemaDocument<Output>;
