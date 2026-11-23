//! Append-only journal, durable blobs and rebuildable projections.

/// Directory containing a workspace's canonical journal.
pub const JOURNAL_DIRECTORY: &str = "journal";

pub mod blob;
pub mod delimited;
pub mod durable;
pub mod import_paths;
pub mod journal;
pub mod opaque;
pub mod pinned;
pub mod projection;
pub mod selectors;
pub mod store;
pub mod workspace;

pub use store::Store;
pub use workspace::Workspace;

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
