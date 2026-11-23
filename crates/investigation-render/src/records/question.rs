//! Question notes: the wording's context, its disposition and answer, and
//! everything that addresses it.

use std::fmt::Write as _;

use investigation_core::identity::Kind;

use super::{Facts, citing};
use crate::RenderError;

const ADDRESSED_BY: &[(Kind, &str)] = &[
    (Kind::Hypothesis, "Hypotheses"),
    (Kind::Option, "Options"),
    (Kind::Criterion, "Criteria"),
    (Kind::Inquiry, "Inquiries"),
    (Kind::Decision, "Decisions"),
];

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.word("Disposition", "disposition");
    f.word("Importance", "importance");
    f.text("Context", "context");
    f.text_present("Scope", "scope");
    f.reference_present("Parent question", "parent")?;
    f.text_present("Answer", "answer");
    f.text_present("Resolution", "resolution_reason");

    f.heading("Addressed by");
    let all = citing(f.notes, f.record, &["question_ref"]);
    let mut any = false;
    for &(kind, label) in ADDRESSED_BY {
        let group: Vec<_> = all
            .iter()
            .filter(|(record, _)| record.header.id.kind() == kind)
            .copied()
            .collect();
        if group.is_empty() {
            continue;
        }
        if any {
            f.out.push('\n');
        }
        any = true;
        let _ = writeln!(f.out, "{label}:\n");
        f.backlinks(&group, "", |_, _| Ok(String::new()))?;
    }
    if !any {
        f.out.push_str(
            "No hypothesis, option, criterion, inquiry or decision addresses this question yet.\n",
        );
    }
    super::matrix::write(f, f.record.header.id)
}
