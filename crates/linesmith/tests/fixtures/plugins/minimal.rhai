// Smallest valid plugin: required `const ID`, no dep header (so the
// declared deps default to Status only), single-run literal render.

const ID = "minimal";

fn render(ctx) {
    #{ runs: [#{ text: "minimal" }] }
}
