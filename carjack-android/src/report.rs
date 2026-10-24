//! The core's log lines of note, and its panics, handed to the app's `Native.Reporter`.

use std::fmt::Write as _;
use std::sync::{Arc, RwLock};

use jni::objects::{GlobalRef, JObject, JValue};
use jni::JavaVM;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// logcat priorities, which the reporter takes; a panic is fatal.
const INFO: i32 = 4;
const WARN: i32 = 5;
const ERROR: i32 = 6;
const FATAL: i32 = 7;

struct Reporter {
    vm: JavaVM,
    object: GlobalRef,
}

static REPORTER: RwLock<Option<Arc<Reporter>>> = RwLock::new(None);

/// Replaces the reporter; `None` stops reports.
pub fn set(vm: JavaVM, object: Option<GlobalRef>) {
    *REPORTER.write().unwrap() = object.map(|object| Arc::new(Reporter { vm, object }));
}

/// Panics are reported as fatal, with a backtrace: on Android stderr goes nowhere.
pub fn catch_panics() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let message = format!("{info}\n{backtrace}");
        crate::logcat::write(FATAL, &message);
        report(FATAL, "panic", &message);
    }));
}

/// Info and up from carjack's own crates; the libraries' lines stay in logcat.
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    ReportLayer.with_filter(
        Targets::new()
            .with_target("libcarjack", Level::INFO)
            .with_target("carjack_android", Level::INFO)
            .with_target("iap2_proto", Level::INFO),
    )
}

struct ReportLayer;

impl<S: Subscriber> Layer<S> for ReportLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let priority = match *event.metadata().level() {
            Level::ERROR => ERROR,
            Level::WARN => WARN,
            Level::INFO => INFO,
            _ => return,
        };
        let mut message = Message::default();
        event.record(&mut message);
        report(priority, event.metadata().target(), &message.0);
    }
}

/// The event's message, then its other fields as `name=value`.
#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }
}

/// Never logs: a failure here would only come straight back.
fn report(priority: i32, target: &str, message: &str) {
    let Some(reporter) = REPORTER.read().ok().and_then(|r| r.clone()) else {
        return;
    };
    let Ok(mut env) = reporter.vm.attach_current_thread_as_daemon() else {
        return;
    };
    // A frame of its own: a daemon thread never detaches, so its local refs would pile up.
    let _ = env.with_local_frame(4, |env| -> jni::errors::Result<()> {
        let target = JObject::from(env.new_string(target)?);
        let message = JObject::from(env.new_string(message)?);
        let _ = env.call_method(
            &reporter.object,
            "report",
            "(ILjava/lang/String;Ljava/lang/String;)V",
            &[JValue::Int(priority), JValue::Object(&target), JValue::Object(&message)],
        );
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        Ok(())
    });
}
