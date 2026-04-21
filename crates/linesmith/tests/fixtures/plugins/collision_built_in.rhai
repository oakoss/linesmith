// Tries to register under a built-in id ("model"). The registry
// rejects this at load time with PluginError::IdCollision so the
// built-in always wins.

const ID = "model";

fn render(ctx) {
    #{ runs: [#{ text: "shadow" }] }
}
