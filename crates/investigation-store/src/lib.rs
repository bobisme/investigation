//! Append-only journal, durable blobs and rebuildable projections.

/// Directory containing a workspace's canonical journal.
pub const JOURNAL_DIRECTORY: &str = "journal";

#[cfg(test)]
mod tests {
    #[test]
    fn journal_directory_is_workspace_relative() {
        let mut components = std::path::Path::new(super::JOURNAL_DIRECTORY).components();
        assert!(matches!(
            components.next(),
            Some(std::path::Component::Normal(_))
        ));
        assert!(components.next().is_none());
    }
}
