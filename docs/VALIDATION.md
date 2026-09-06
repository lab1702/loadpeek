# Validation

Validated on Linux x86_64 with Rust 1.98.0, eframe 0.36.1, and an X11 desktop on 2026-09-06.

- 34 unit tests pass: Linux counter accounting, missing inputs and resets, timestamped history, persistent settings, chart scales and keyboard history access, and WCAG contrast pairings.
- `cargo fmt --check` and Clippy across all targets/features pass without warnings.
- An optimized release executable builds successfully.
- Native GUI checks exercise keyboard navigation across all six pages and inspect the actual AccessKit tree. Current values, associated control labels, active navigation, and historical time-cursor values are exposed.
- Live refresh checks collect five samples over 2.2 seconds at a 0.5-second interval. At a 5-second interval, no sample arrives after two seconds and one arrives after the interval. Pause holds the sample count; resume resets history.
- Interface scaling and refresh preferences persist in an isolated test configuration.
- Visual checks cover 1440×1000, 640×480, and a 640×480 window at 200% scale. Compact navigation and page scrolling keep content reachable; settings scroll within the window.
- Live collection was exercised on a 24-thread AMD system with physical RAM, swap, disk counters, network interfaces, and ten temperature sensors. A restricted container view separately verified unavailable network handling.

The inspection feature is opt-in (`cargo build --features inspection`). It is disabled in the normal release build; testing used a loopback-only port. No metrics or history are transmitted by the normal application.

Contrast and programmatic accessibility checks are not a full WCAG conformance assessment. Manual Orca/screen-reader testing and a Wayland session were not available in this validation environment.
