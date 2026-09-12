//! Drive API: files, folders, and upload body handling. TODO(Wave2).

pub mod files;
pub mod folders;
pub mod upload_body;

pub(crate) fn validate_name(name: &str, kind: &'static str) -> Result<(), crate::error::AppError> {
    if name.trim().is_empty() {
        return Err(crate::error::AppError::Validation(format!(
            "{kind} name must not be blank"
        )));
    }
    if name.chars().count() > 255 {
        return Err(crate::error::AppError::Validation(format!(
            "{kind} name is too long"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    #[rstest]
    #[case("", Some("must not be blank"))]
    #[case("  \n", Some("must not be blank"))]
    #[case(&"a".repeat(255), None)]
    #[case(&"a".repeat(256), Some("is too long"))]
    #[case(&"あ".repeat(255), None)]
    #[case(&"あ".repeat(256), Some("is too long"))]
    fn validate_name_enforces_character_limits(
        #[case] name: &str,
        #[case] expected: Option<&str>,
        #[values("file", "folder")] kind: &'static str,
    ) {
        // Given: a file or folder name at a validation boundary.
        // When
        let result = super::validate_name(name, kind);
        // Then
        match expected {
            Some(message) => assert!(matches!(
                result,
                Err(crate::error::AppError::Validation(actual))
                    if actual == format!("{kind} name {message}")
            )),
            None => assert!(result.is_ok()),
        }
    }
}
