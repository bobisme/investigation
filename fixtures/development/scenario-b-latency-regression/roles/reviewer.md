# Reviewer role

Check that no conclusion rests on the debug-logging run, on a sequential A-then-B
pair, or on a percentile whose denominator is only the completed requests.
Confirm that "no improvement detected" under a failed monitor or a short window
is kept as inconclusive, never as equality, and that no p-value, power or
interval was produced by the investigation itself.

Confirm the equivalence verdict follows the interval against the margin frozen
before results: [-2, +3] sits inside +-5 ms; [-8, +3] does not. Confirm that a
zero exit code was never taken as a valid measurement.
