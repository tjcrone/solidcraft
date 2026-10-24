//! Saves the design's print settings: `print.overlay`, the slicer keys (OrcaSlicer names) the
//! design overrides on top of the printer, filament and process files it is sliced with, and
//! `print.placement`, where the design sits on the bed (`crate::print_settings`). An older
//! design has no `print` member and slices with its files' values alone, in the middle of the
//! bed, so no step rewrites it. The last release refuses the newer format number.
