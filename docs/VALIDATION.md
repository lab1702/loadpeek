# Validation

Validated on Linux x86_64 with Rust 1.98.0, eframe 0.36.1, and an X11 desktop on 2026-09-06.

- 73 tests pass: Linux counter accounting, CPU frequency statistics, missing inputs and resets, timestamped history, persistent settings, chart scales and keyboard history access, process parsing and PID reuse/exit races, sorting/filtering and keyboard navigation, and WCAG contrast pairings. Review regressions cover collection during hidden-window logic ticks, pause/resume with an in-flight sample, retained disk/interface selection, sensor faults and recovery, PECI control targets, scrolling to focused charts, and trailing empty command arguments at truncation boundaries.
- `cargo fmt --check` and Clippy across all targets/features pass without warnings.
- An optimized release executable builds successfully.
- Native GUI checks exercise keyboard navigation across all seven pages and inspect the actual AccessKit tree. Current values, associated control labels, active navigation, historical time-cursor values, process rows, and sort state are exposed.
- Live refresh checks collect five samples over 2.2 seconds at a 0.5-second interval. At a 5-second interval, no sample arrives after two seconds and one arrives after the interval. Pause holds the sample count; resume resets history.
- Interface scaling and refresh preferences persist in an isolated test configuration.
- Visual checks cover 1440×1000, 640×480, and a 640×480 window at 200% scale. Compact navigation and page scrolling keep content reachable; settings scroll within the window.
- Live collection was exercised on a 24-thread AMD system with physical RAM, swap, disk counters, network interfaces, and ten temperature sensors. A restricted container view separately verified unavailable network handling.
- Process collection was checked against a bounded CPU-busy child with allocated memory: approximately 100% of one core, expected name/user and resident memory, and removal after exit. The native process page displayed approximately 520 host processes.
- Process GUI checks cover PID sorting in both directions, memory/CPU sort selection, command filtering and empty results, selected process details, and keyboard movement to the first/last row and by page. At 640×480 and 200% scale, page scrolling reaches the table and its process rows remain keyboard-operable; horizontal scrolling exposes the remaining columns.

After the code-review fixes, the full test suite, formatting, Clippy across all targets/features, and optimized release build were rerun successfully. The new regressions use filesystem fixtures, deterministic collector-thread synchronization, and headless egui logic/layout checks. The native desktop checks listed above preceded these fixes and were not repeated for this follow-up.

The latest whole-codebase review added eight regression tests. They cover replacement network interfaces reusing a name, missing or invalid interface identities and rate recovery, legacy hwmon driver names, per-device AMD Tdie preference with Tctl fallback and CCD readings, distinct accessible chart names and historical values, and keyboard focus visibility through nested process scroll areas at 640×480 and 320×240 logical sizes. The chart-name and process-scroll regressions were confirmed to fail before their fixes. Live metric collection was also rerun successfully in the restricted environment; native screen-reader and Wayland testing remain unperformed.

The inspection feature is opt-in (`cargo build --features inspection`). It is disabled in the normal release build; testing used a loopback-only port. No metrics or history are transmitted by the normal application.

Contrast and programmatic accessibility checks are not a full WCAG conformance assessment. Manual Orca/screen-reader testing and a Wayland session were not available in this validation environment.
