#[derive(Debug, thiserror::Error)]
pub enum AdkError {
    #[error("Agent error: {0}")]
    Agent(String),

    #[error("Model error: {0}")]
    Model(String),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Artifact error: {0}")]
    Artifact(String),

    #[error("Memory error: {0}")]
    Memory(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AdkError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = AdkError::Agent("test error".to_string());
        assert_eq!(err.to_string(), "Agent error: test error");
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let adk_err: AdkError = io_err.into();
        assert!(matches!(adk_err, AdkError::Io(_)));
    }

    #[test]
    fn test_result_type() {
        let ok_result: Result<i32> = Ok(42);
        assert_eq!(ok_result.unwrap(), 42);

        let err_result: Result<i32> = Err(AdkError::Config("invalid".to_string()));
        assert!(err_result.is_err());
    }
}
