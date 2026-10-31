/// Emit a bounded, infrequent diagnostic with its subscriber but no span stack.
/// Per-layer filters can otherwise skip a root span and restore private context.
/// Thread startup or logging failure drops the diagnostic without a fallback.
pub(crate) fn without_span_context(record: impl FnOnce() + Send) {
    if !super::enabled() {
        return;
    }
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    let _ = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("cairn-diagnostics".into())
            .spawn_scoped(scope, move || {
                tracing::dispatcher::with_default(&dispatch, record);
            })
            .map(std::thread::ScopedJoinHandle::join)
    });
}
