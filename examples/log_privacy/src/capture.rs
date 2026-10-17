//! A tracing layer that records every span field and event field at every level.
//!
//! Each record is one line: `EVENT <level> <target>: <fields>` for a log event, or
//! `SPAN <name>: <fields>` when a span is created or a field is recorded on it. A scenario
//! clears the buffer, runs, and searches the lines, so whatever any crate logs or puts on a
//! span is visible, including TRACE events and span attributes an exporter would ship.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<String>>>);

impl Capture {
    /// Installs the capture as the global subscriber.
    pub fn install() -> anyhow::Result<Self> {
        let capture = Self::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(capture.clone()),
        )?;
        Ok(capture)
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(self.0, " {}={value}", field.name());
    }
}

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = Fields(format!("SPAN {}:", attrs.metadata().name()));
        attrs.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let name = ctx.span(id).map(|span| span.name()).unwrap_or("?");
        let mut fields = Fields(format!("SPAN {name}:"));
        values.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut fields = Fields(format!("EVENT {} {}:", metadata.level(), metadata.target()));
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}
