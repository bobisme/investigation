//! Domain records, lifecycle validation and versioned contracts.

/// The name used by the standalone command.
pub const PROGRAM_NAME: &str = "investigation";

pub mod absence_gate;
pub mod attempt;
pub mod authored_text;
pub mod basis;
pub mod blindspot;
pub mod blindspot_review;
pub mod case;
mod case_ops;
pub mod catalog;
pub mod classification;
mod classification_ops;
pub mod command;
pub mod confidence;
pub mod coverage;
mod coverage_ops;
pub mod decision_ops;
pub mod derivation;
pub mod evaluate;
pub mod evaluate_ops;
pub mod evidence;
pub mod evidence_reuse;
pub mod explain;
mod explain_ops;
pub mod external_ref;
pub mod freshness;
pub mod gap_explain;
pub mod gap_page;
pub mod gap_rules;
pub mod guidance;
pub mod header;
pub mod identity;
pub mod input;
pub mod lessons;
pub mod lessons_ops;
pub mod likelihood;
pub mod local_path;
pub mod measurement_fields;
pub mod measurement_flags;
pub mod measurement_ops;
pub mod method_pack;
pub mod method_pack_ops;
pub mod next_rank;
pub mod objection_policy;
pub mod observation_ops;
pub mod origin_ops;
pub mod pack_policy;
pub mod producer_run;
pub mod retrospective;
pub mod retrospective_digest;
mod retrospective_ops;
pub mod review;
pub mod review_independence;
pub mod review_ops;
pub mod review_packet;
pub mod search_coverage;
pub mod search_text;
pub mod source_ops;
pub mod stall;
pub mod state;
pub mod synopsis;
pub mod weighting;
pub mod wire;

#[cfg(test)]
mod tests {
    #[test]
    fn program_name_matches_the_public_command() {
        assert_eq!(super::PROGRAM_NAME, "investigation");
    }
}
