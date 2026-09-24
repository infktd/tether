//! Tries to use the network directly, for the runtime's sandbox tests.

wit_bindgen::generate!({
    world: "net",
    path: "wit",
});

struct Net;

impl Guest for Net {
    fn connect(addr: String) -> String {
        match std::net::TcpStream::connect(addr.as_str()) {
            Ok(_) => "connected".to_owned(),
            Err(err) => format!("refused: {err}"),
        }
    }
}

export!(Net);
