# Scenario C: queue architecture choice

All sources are synthetic, local, and provider-neutral. The baseline compares
three architecture options under one declared recoverable-job failure model.
Numbers are small illustrative measurements, not general performance or cost
claims. Each manifest gives byte digests and the intended source role.

The baseline hard requirement is zero loss of an acknowledged enqueue after an
abrupt process or single-host stop, with recovery visible within 300 seconds.
Cost projections depend on an explicitly unverified traffic estimate. Team
complexity includes an author's preference, not measured evidence. An unknown
cell remains unknown even if another option is described as “best”.

`strict-threshold/` is a separate adversarial variant. It copies the baseline
source bytes, retains the accepted decision record, and changes only the hard
recovery-time threshold from 300 seconds to 60 seconds. The old decision should
be marked `needs_review`; the sources and earlier measurements stay intact.

## By-role reading guides

- `roles/investigator.md` describes the source-first comparison sequence.
- `roles/reviewer.md` lists the uncertainty, preference/evidence distinction,
  and criterion-change checks.
