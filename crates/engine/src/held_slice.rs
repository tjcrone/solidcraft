//! The slice the session holds after `print.slice`: the G-code, the estimates and the toolpaths of
//! the last run, so a preview, a save and an upload read the same slice instead of running the
//! engine again. It is session state like the render jobs, never part of the design: it is not
//! saved, not undone, and another design opened in the session drops it.

use std::sync::Arc;

use solidcraft_doc::Document;
use solidcraft_manufacture::Sliced;

/// What `print.slice` produced and what it was made from.
#[derive(Debug)]
pub struct HeldSlice {
    pub sliced: Sliced,
    /// The design as it was when sliced; the slice is stale once the session's design differs.
    pub doc: Arc<Document>,
    /// The bodies sliced, in the design's order; empty when every body was.
    pub bodies: Vec<String>,
    /// The settings the engine read, as one JSON object text.
    pub config: String,
    /// The engine's name and version.
    pub slicer: String,
}

impl HeldSlice {
    /// The design has changed since this slice: its preview and G-code no longer show the design.
    pub fn stale(&self, current: &Arc<Document>) -> bool {
        !Arc::ptr_eq(&self.doc, current) && *self.doc != **current
    }
}

impl crate::Session {
    /// The last slice made in this session, if any.
    pub fn print_slice(&self) -> Option<&Arc<HeldSlice>> {
        self.print_slice.as_ref()
    }

    pub(crate) fn set_print_slice(&mut self, held: Option<HeldSlice>) {
        self.print_slice = held.map(Arc::new);
    }
}
