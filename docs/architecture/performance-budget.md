# Performance Budget

This document defines the first runtime budget target for the app while the architecture is being split into a headless runtime and a thinner UI layer.
The budget is an acceptance gate for performance work in this stage, not a nice-to-have note.

## Baseline Target

- `20` open terminals
- `6` visible terminals
- `3` terminals producing sustained output

## Acceptance Criteria

The budget is not a public benchmark claim. It is a required engineering constraint for the runtime and render tiers:

- open terminals should stay cheap when hidden or offscreen
- visible terminals should degrade to lighter render tiers when they are not focused
- bursty output should be coalesced instead of repainted synchronously per event
- restored terminals should remain detached until the user focuses or interacts with them
- drag/resize interaction should defer orchestration scans and avoid unnecessary runtime work

## Instrumentation

Development builds should expose enough internal signal to explain regressions while keeping telemetry invisible to end users:

- frame time
- visible panel count
- attached vs detached PTY sessions
- render tier counts per frame
- cache hits vs misses
- repaint reason
- orchestration scan duration

## Scenario Coverage

The stage is not complete until the baseline shape is covered by repeatable smoke tests:

- `1` open / `1` visible / `0` streaming
- `4` open / `2` visible / `1` streaming
- `20` open / `6` visible / `3` streaming

## Acceptance Rule

The implementation is acceptable only when the app can keep the baseline target stable on moderate hardware without changing the visual language or product behavior.

## Notes

- This budget is intentionally conservative.
- The runtime can exceed it, but regressions should be measured against it.
- Future phases may tighten latency targets once the runtime layer is isolated.
