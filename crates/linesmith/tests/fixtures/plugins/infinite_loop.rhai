// A tight `loop {}` that trips the engine's `MAX_OPERATIONS` ceiling
// in microseconds — long before the 50ms wallclock deadline could
// fire. The deadline path is exercised at the unit-test layer
// (`src/plugins/engine.rs::past_deadline_aborts_long_running_script`,
// `src/plugins/segment.rs::deadline_abort_surfaces_clear_segment_error`)
// because injecting a past deadline through `RhaiSegment::render` is
// impossible: its RAII guard overwrites the thread-local before
// `call_fn`. This fixture pins the op-limit branch only.

const ID = "infinite_loop";

fn render(ctx) {
    loop {}
}
