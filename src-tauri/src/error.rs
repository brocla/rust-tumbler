use pdfium_render::prelude::PdfiumError;

/// Internal error type for command helper functions. Unlike a bare `String`,
/// callers can match on the variant to distinguish error categories — e.g. a
/// missing document, a corrupt/unsupported PDF, or a filesystem error — which
/// matters if different categories ever need different UI treatment.
/// `#[tauri::command]` functions convert this to `String` at the IPC
/// boundary via `From<AppError> for String`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Document not found: {0}")]
    NotFound(String),

    #[error("Lock error: {0}")]
    Lock(String),

    #[error("{message}: {}", crate::describe_pdfium_error(.cause))]
    Pdfium { message: String, cause: PdfiumError },

    #[error("{message}: {cause}")]
    Io { message: String, cause: std::io::Error },

    #[error("{message}: {cause}")]
    Lopdf { message: String, cause: lopdf::Error },

    #[error("{0}")]
    Other(String),
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Other(s)
    }
}

impl AppError {
    pub fn pdfium(message: impl Into<String>, cause: PdfiumError) -> Self {
        AppError::Pdfium {
            message: message.into(),
            cause,
        }
    }

    pub fn io(message: impl Into<String>, cause: std::io::Error) -> Self {
        AppError::Io {
            message: message.into(),
            cause,
        }
    }

    pub fn lopdf(message: impl Into<String>, cause: lopdf::Error) -> Self {
        AppError::Lopdf {
            message: message.into(),
            cause,
        }
    }
}

impl From<AppError> for String {
    fn from(err: AppError) -> Self {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_includes_doc_id() {
        let err = AppError::NotFound("abc-123".to_string());
        assert_eq!(err.to_string(), "Document not found: abc-123");
    }

    #[test]
    fn lock_includes_detail() {
        let err = AppError::Lock("poisoned".to_string());
        assert_eq!(err.to_string(), "Lock error: poisoned");
    }

    #[test]
    fn io_includes_message_and_cause() {
        let cause = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = AppError::io("Failed to replace PDF with updated copy", cause);
        assert_eq!(
            err.to_string(),
            "Failed to replace PDF with updated copy: access denied"
        );
    }

    #[test]
    fn other_displays_message_as_is_and_converts_to_string() {
        let err: AppError = "boom".to_string().into();
        assert_eq!(err.to_string(), "boom");
        assert_eq!(String::from(err), "boom".to_string());
    }
}
