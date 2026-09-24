//! A minimal plugin. Build with
//! `cargo build -p hello-plugin --target wasm32-wasip2 --release`,
//! which emits a WASM component directly.

wit_bindgen::generate!({
    world: "hello",
    path: "wit",
});

struct HelloPlugin;

impl Guest for HelloPlugin {
    fn greet(name: String) -> String {
        format!("o7 {name}")
    }
}

export!(HelloPlugin);
