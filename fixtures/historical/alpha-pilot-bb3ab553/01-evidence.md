# Evidence Summary

## Source File: /work/source.txt

### Requirements (Hard Constraints):
- Search p95 latency must be under 130 ms
- One full re-index per night required

### Options Evaluated:

#### Central-index
| Metric | Value | Status |
|--------|-------|--------|
| Search p95 | 72 ms | ✓ Passes (< 130ms) |
| Re-index time | 44 minutes | - |
| Operator-hours/week | 1 hour | Lower ongoing work |

#### Local-shard  
| Metric | Value | Status |
|--------|-------|--------|
| Search p95 | 113 ms | ✓ Passes (< 130ms) |
| Re-index time | 71 minutes | - |
| Operator-hours/week | 3 hours | Higher ongoing work |

### Stated Preference:
- Engineering prefers lower ongoing work among eligible options
- Pre-review preference is Central-index (both choices inside initial timing criteria)

### Decision Logic:
Both options meet the hard requirements. The deciding factor per stated preference is "lower ongoing work."
Central-index requires 1 operator-hour/week vs Local-shard's 3 hours/week.

EOF && cat /work/casework/01-evidence.md
