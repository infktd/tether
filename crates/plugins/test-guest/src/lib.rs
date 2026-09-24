//! Misbehaves on request, for the runtime's limit tests.

wit_bindgen::generate!({
    world: "limits",
    path: "wit",
});

struct Limits;

impl Guest for Limits {
    fn spin() {
        let mut n: u64 = 0;
        loop {
            n = std::hint::black_box(n.wrapping_add(1));
        }
    }

    fn allocate(mib: u32) -> u64 {
        let mut held: Vec<Vec<u8>> = Vec::new();
        for _ in 0..mib {
            // Touch every page so the memory is really used.
            held.push(vec![1u8; 1024 * 1024]);
        }
        held.iter().map(|chunk| chunk.len() as u64).sum()
    }

    #[allow(clippy::panic)]
    fn crash() {
        panic!("the test guest crashes on purpose");
    }

    fn recurse(depth: u32) -> u32 {
        if depth == 0 {
            return 0;
        }
        // Enough stack per frame that a deep call overflows.
        let pad = std::hint::black_box([depth; 64]);
        1 + Self::recurse(depth - 1) + pad[0] - depth
    }

    fn echo(text: String) -> String {
        text
    }
}

export!(Limits);
