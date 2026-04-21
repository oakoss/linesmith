// Compile-time failure: unterminated string literal. The registry
// rejects this with PluginError::Compile at load time; the file
// never reaches the segment runtime.

const ID = "syntax_error";

fn render(ctx) {
    #{ runs: [#{ text: "never compiles }] }
}
