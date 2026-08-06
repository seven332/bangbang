use std::cell::Cell;

#[test]
fn trace_scope_macro_evaluates_only_in_tracing_builds() {
    let evaluations = Cell::new(0_u32);

    bangbang_runtime::bangbang_trace_scope!(
        {
            evaluations.set(evaluations.get() + 1);
            bangbang_runtime::logger::TraceLogger::default()
        },
        "bangbang_runtime::feature_test",
        "macro_evaluation",
    );

    #[cfg(feature = "tracing")]
    assert_eq!(evaluations.get(), 1);
    #[cfg(not(feature = "tracing"))]
    assert_eq!(evaluations.get(), 0);
}
