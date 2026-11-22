//! Neutral ingestion boundary for independent producers.

/// Returns the name of the recorder implementation.
pub fn implementation_name() -> &'static str {
    investigation_core::PROGRAM_NAME
}

#[cfg(test)]
mod tests {
    #[test]
    fn implementation_identifies_itself() {
        assert_eq!(super::implementation_name(), "investigation");
    }
}
