// Returns `()` (hide) when `ctx.status.rate_limits` is unset; renders
// otherwise. The Segment trait has no separate visibility hook;
// returning `()` from `render` is the same observable behavior.

const ID = "visibility_via_render";

fn render(ctx) {
    if ctx.status.rate_limits == () {
        ()
    } else {
        #{ runs: [#{ text: "rate-limit-aware" }] }
    }
}
