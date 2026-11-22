//! Domain records, lifecycle validation and versioned contracts.

/// The name used by the standalone command.
pub const PROGRAM_NAME: &str = "investigation";

pub mod header;
pub mod identity;
pub mod input;

#[cfg(test)]
mod tests {
    #[test]
    fn program_name_matches_the_public_command() {
        assert_eq!(super::PROGRAM_NAME, "investigation");
    }
}
