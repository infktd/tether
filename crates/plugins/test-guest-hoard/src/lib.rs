//! Hoards resource handles, for the runtime's sandbox tests.

wit_bindgen::generate!({
    world: "hoarder",
    path: "wit",
});

use exports::tether::test_guest_hoard::hoard::{Guest, GuestToken, Token};

struct Hoarder;

struct Held;

impl GuestToken for Held {
    fn new() -> Self {
        Held
    }
}

impl Guest for Hoarder {
    type Token = Held;

    fn fill(count: u32) -> u32 {
        for _ in 0..count {
            std::mem::forget(Token::new(Held));
        }
        count
    }
}

export!(Hoarder);
