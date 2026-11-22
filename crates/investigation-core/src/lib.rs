//! Domain records, lifecycle validation and versioned contracts.

/// The name used by the standalone command.
pub const PROGRAM_NAME: &str = "investigation";

pub mod case;
pub mod catalog;
pub mod command;
pub mod evaluate;
pub mod evidence;
pub mod explain;
pub mod header;
pub mod identity;
pub mod input;
pub mod retrospective;
pub mod review;
pub mod state;
pub mod wire;

#[cfg(test)]
mod tests {
    #[test]
    fn program_name_matches_the_public_command() {
        assert_eq!(super::PROGRAM_NAME, "investigation");
    }
}
