//! Slow-handler reports as diagnostics of the logging core (the `logging` feature).
#![cfg(feature = "logging")]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use event::{EventBus, EventPriority};
use log_core::{Diagnostic, Severity};

struct Ping;

fn field<'a>(diagnostic: &'a Diagnostic, key: &str) -> &'a str {
    diagnostic
        .extra
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("no extra field named {key}"))
}

#[test]
fn a_slow_handler_becomes_a_warning_diagnostic() {
    let bus = EventBus::new();
    let diagnostics: Arc<Mutex<Vec<Diagnostic>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&diagnostics);
    bus.set_slow_handler_hook(Duration::from_millis(5), move |report| {
        sink.lock().unwrap().push(report.to_diagnostic());
    });
    let subscription = bus.subscribe_with_priority(EventPriority::High, |_: &Ping| {
        thread::sleep(Duration::from_millis(20));
    });

    bus.publish(&Ping);

    let diagnostics = diagnostics.lock().unwrap();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.code.producer, "CV-EVENT");
    assert_eq!(diagnostic.code.category, "SLOW");
    assert_eq!(diagnostic.code.number, 1);
    assert_eq!(diagnostic.code.as_string(), "CV-EVENT-SLOW-0001");
    assert_eq!(diagnostic.severity, Severity::Warning);
    assert_eq!(diagnostic.module.as_deref(), Some("event"));
    assert!(diagnostic.message.contains("observer"));
    assert!(diagnostic.message.contains("took"));
    assert!(field(diagnostic, "event_type").ends_with("Ping"));
    assert_eq!(
        field(diagnostic, "handler_id"),
        subscription.id().to_string()
    );
    assert_eq!(field(diagnostic, "kind"), "observer");
    assert_eq!(field(diagnostic, "priority"), "High");
    assert_eq!(field(diagnostic, "threshold_us"), "5000");
    let elapsed: u128 = field(diagnostic, "elapsed_us").parse().unwrap();
    assert!(elapsed >= 20_000);
}
