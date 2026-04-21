// Reads a configured key from `[segments.uses_ctx_config]`.
// Renders the config-supplied label verbatim.

const ID = "uses_ctx_config";

fn render(ctx) {
    #{ runs: [#{ text: ctx.config.label }] }
}
