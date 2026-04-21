// @data_deps = ["usage"]
//
// Declares the `usage` source. Today's lazy accessor returns the
// pre-cascade sentinel `Err(UsageError::Jsonl(JsonlError::NoEntries))`;
// the rhai ctx mirror delegates `.code()` through to the inner
// variant, so plugins exercising the dep see the tagged error map:
// `#{ kind: "error", error: "NoEntries" }`. Renders the error code
// so a test can pin both the gating path (ctx.usage is present) and
// the error-shape contract.

const ID = "declares_usage";

fn render(ctx) {
    let label = if ctx.usage.kind == "error" {
        ctx.usage.error
    } else {
        "ok"
    };
    #{ runs: [#{ text: label }] }
}
