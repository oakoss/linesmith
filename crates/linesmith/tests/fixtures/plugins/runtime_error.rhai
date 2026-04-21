// Compiles cleanly but errors at render time (division by zero).
// The segment wrapper maps this to SegmentError via the generic
// classifier branch (no deadline, no shape mismatch).

const ID = "runtime_error";

fn render(ctx) {
    let n = 1 / 0;
    #{ runs: [#{ text: `${n}` }] }
}
