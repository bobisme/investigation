//! Domain records, lifecycle validation and versioned contracts.

/// The name used by the standalone command.
pub const PROGRAM_NAME: &str = "investigation";

pub mod basis;
pub mod case;
mod case_ops;
pub mod catalog;
pub mod classification;
mod classification_ops;
pub mod command;
pub mod decision_ops;
pub mod evaluate;
pub mod evaluate_ops;
pub mod evidence;
pub mod explain;
mod explain_ops;
pub mod freshness;
pub mod gap_explain;
pub mod gap_page;
pub mod gap_rules;
pub mod header;
pub mod identity;
pub mod input;
pub mod measurement_ops;
pub mod method_pack;
pub mod method_pack_ops;
pub mod observation_ops;
pub mod retrospective;
mod retrospective_ops;
pub mod review;
pub mod review_ops;
pub mod state;
pub mod wire;

#[cfg(test)]
mod tests {
    #[test]
    fn program_name_matches_the_public_command() {
        assert_eq!(super::PROGRAM_NAME, "investigation");
    }
}
