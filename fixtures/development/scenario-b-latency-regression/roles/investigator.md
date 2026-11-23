# Investigator role

Start from each run's `measurement.json`, not its envelope: check unit, warm-up,
window, trial order, load model and attempt counts before any percentile. Then
compare environment identity between the runs you intend to set side by side.
Note which differences (instrumentation, warm-up, workload mix, run order) would
explain a p99 difference without the allocator or the optimization.

Treat the producer's p99 as the producer's claim. Recompute it from `output.json`
and ask what it leaves out: slow requests that never finished, requests delayed
by a closed-loop generator, samples from a window that was too short. Bound any
conclusion to the workload that was measured. Where a variant supplies an
interval, use that interval as given and do not derive another.
