//! The waterfall contract: registration order, short-circuit, and
//! reversible middleware removal.
//!
//! The futures are driven by `cordis::utils::block_on`, the same
//! runtime-free parker the framework uses, so these tests add no async
//! runtime to the kernel's dev graph.

use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use cordis::utils::block_on;
use elysper_kernel::{
    Context,
    EventValue,
    Inner,
    Outcome,
    Waterfall,
    WaterfallCtx,
    WaterfallError,
};

type Log = Arc<Mutex<Vec<&'static str>>>;

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn recorded(log: &Log) -> Vec<&'static str> {
    log.lock().expect("log").clone()
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn request() -> Arc<WaterfallCtx> {
    Arc::new(WaterfallCtx::new(
        "test/event",
        EventValue::new("payload".to_owned()),
        Context::new(),
    ))
}

fn inner(calls: Arc<AtomicUsize>, outcome: fn() -> Outcome) -> Inner {
    Arc::new(move |_request| {
        calls.fetch_add(1, Ordering::SeqCst);
        let outcome = outcome();
        Box::pin(async move { Ok(outcome) })
    })
}

#[test]
fn short_circuit_skips_inner_and_later_middleware() {
    let ctx = Context::new();
    let waterfall = Waterfall::new(&ctx);
    let later = counter();
    let inner_calls = counter();

    let _short = waterfall
        .register(Arc::new(|_request, _next| {
            Box::pin(async {
                Ok(Outcome::ShortCircuit(EventValue::new("blocked".to_owned())))
            })
        }))
        .expect("register short circuit");
    let _later = waterfall
        .register(Arc::new({
            let later = Arc::clone(&later);
            move |request, next| {
                later.fetch_add(1, Ordering::SeqCst);
                next(request)
            }
        }))
        .expect("register later middleware");

    let outcome = block_on(waterfall.run(
        request(),
        inner(Arc::clone(&inner_calls), || Outcome::Continue),
    ))
    .expect("run");

    match outcome {
        Outcome::ShortCircuit(value) => {
            let payload = value.downcast::<String>().expect("string payload");
            assert_eq!(payload.as_str(), "blocked");
        }
        Outcome::Continue => panic!("expected a short circuit"),
    }
    assert_eq!(later.load(Ordering::SeqCst), 0, "later middleware skipped");
    assert_eq!(inner_calls.load(Ordering::SeqCst), 0, "inner skipped");
}

#[test]
fn continue_runs_inner_once() {
    let ctx = Context::new();
    let waterfall = Waterfall::new(&ctx);
    let inner_calls = counter();
    let observed = Arc::new(Mutex::new(None::<String>));

    let _first = waterfall
        .register(Arc::new({
            let observed = Arc::clone(&observed);
            move |request, next| {
                assert_eq!(request.event(), "test/event");
                *observed.lock().expect("observed") = request
                    .payload()
                    .downcast::<String>()
                    .ok()
                    .map(|value| value.as_str().to_owned());
                assert!(
                    request
                        .services()
                        .get::<AtomicUsize>("missing")
                        .expect("read service")
                        .is_none(),
                    "the request exposes the isolated service view"
                );
                next(request)
            }
        }))
        .expect("register first");
    let _second = waterfall
        .register(Arc::new(|request, next| next(request)))
        .expect("register second");

    let outcome = block_on(waterfall.run(
        request(),
        inner(Arc::clone(&inner_calls), || Outcome::Continue),
    ))
    .expect("run");

    assert!(matches!(outcome, Outcome::Continue));
    assert_eq!(inner_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        observed.lock().expect("observed").as_deref(),
        Some("payload")
    );
}

#[test]
fn first_registered_is_outermost() {
    let ctx = Context::new();
    let waterfall = Waterfall::new(&ctx);
    let log = log();

    let _first = waterfall
        .register(Arc::new({
            let log = Arc::clone(&log);
            move |request, next| {
                log.lock().expect("log").push("first");
                next(request)
            }
        }))
        .expect("register first");
    let _second = waterfall
        .register(Arc::new({
            let log = Arc::clone(&log);
            move |request, next| {
                log.lock().expect("log").push("second");
                next(request)
            }
        }))
        .expect("register second");

    let inner = Arc::new({
        let log = Arc::clone(&log);
        move |_request| {
            log.lock().expect("log").push("inner");
            Box::pin(async { Ok(Outcome::Continue) })
                as cordis::utils::BoxFuture<Result<Outcome, WaterfallError>>
        }
    }) as Inner;

    block_on(waterfall.run(request(), inner)).expect("run");
    assert_eq!(recorded(&log), vec!["first", "second", "inner"]);
}

#[test]
fn dropping_registration_removes_middleware() {
    let ctx = Context::new();
    let waterfall = Waterfall::new(&ctx);
    let middleware_calls = counter();
    let inner_calls = counter();

    let registration = waterfall
        .register(Arc::new({
            let middleware_calls = Arc::clone(&middleware_calls);
            move |request, next| {
                middleware_calls.fetch_add(1, Ordering::SeqCst);
                next(request)
            }
        }))
        .expect("register");
    assert_eq!(waterfall.len(), 1);

    block_on(waterfall.run(
        request(),
        inner(Arc::clone(&inner_calls), || Outcome::Continue),
    ))
    .expect("first run");
    assert_eq!(middleware_calls.load(Ordering::SeqCst), 1);
    assert_eq!(inner_calls.load(Ordering::SeqCst), 1);

    drop(registration);
    assert!(waterfall.is_empty(), "registration revoked");

    block_on(waterfall.run(
        request(),
        inner(Arc::clone(&inner_calls), || Outcome::Continue),
    ))
    .expect("second run");
    assert_eq!(
        middleware_calls.load(Ordering::SeqCst),
        1,
        "revoked middleware is not called"
    );
    assert_eq!(inner_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn empty_chain_calls_inner_once() {
    let waterfall = Waterfall::new(&Context::new());
    assert!(waterfall.is_empty());
    assert_eq!(waterfall.len(), 0);

    let inner_calls = counter();
    let outcome = block_on(waterfall.run(
        request(),
        inner(Arc::clone(&inner_calls), || Outcome::Continue),
    ))
    .expect("run");

    assert!(matches!(outcome, Outcome::Continue));
    assert_eq!(inner_calls.load(Ordering::SeqCst), 1);
}
