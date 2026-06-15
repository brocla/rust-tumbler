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

    /// `Pdfium { message, cause }` should combine the caller's message with
    /// `describe_pdfium_error`'s rendering of the cause, on a single line
    /// (pdfium-render's own `Display`/`Debug` for `PdfiumError` can span
    /// multiple lines, which would look broken in a dialog/log line).
    #[test]
    fn pdfium_variant_includes_message_and_single_line_cause() {
        let pdfium = crate::test_pdfium();
        let missing = std::env::temp_dir().join("tumbler_does_not_exist.pdf");

        let cause = pdfium
            .load_pdf_from_file(missing.to_str().unwrap(), None)
            .expect_err("expected pdfium error");

        let err = AppError::pdfium("Failed to load PDF", cause);
        let display = err.to_string();

        assert!(display.starts_with("Failed to load PDF: "));
        assert!(!display.contains('\n'), "display should be a single line: {display}");
    }

    /// For `PdfiumLibraryInternalError`, `describe_pdfium_error` should
    /// format the *inner* error (e.g. "FormatError") rather than
    /// pdfium-render's multi-line `Debug` for the outer
    /// `PdfiumLibraryInternalError(...)` wrapper.
    #[test]
    fn describe_pdfium_error_unwraps_internal_error_to_inner_value() {
        let pdfium = crate::test_pdfium();
        let garbage = std::env::temp_dir().join("tumbler_garbage.pdf");
        std::fs::write(&garbage, b"not a pdf").expect("write garbage file");

        let cause = pdfium
            .load_pdf_from_file(garbage.to_str().unwrap(), None)
            .expect_err("expected pdfium error");

        let described = crate::describe_pdfium_error(&cause);
        assert_eq!(described, "FormatError");

        std::fs::remove_file(&garbage).ok();
    }

    #[test]
    fn lopdf_variant_includes_message_and_cause() {
        let missing = std::env::temp_dir().join("tumbler_does_not_exist.pdf");
        let cause = lopdf::Document::load(&missing).expect_err("expected lopdf error");

        let err = AppError::lopdf("Failed to open PDF", cause);
        let display = err.to_string();

        assert!(display.starts_with("Failed to open PDF: "));
        assert!(display.len() > "Failed to open PDF: ".len());
    }
}
