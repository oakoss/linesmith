// Returns `()` (hide) when `ctx.status.cost` is unset; renders
// otherwise. The Segment trait has no separate visibility hook;
// returning `()` from `render` is the same observable behavior.

const ID = "visibility_via_render";

fn render(ctx) {
    if ctx.status.cost == () {
        ()
    } else {
        #{ runs: [#{ text: "cost-aware" }] }
    }
}
