//! A web server for remote images: each request answered as the test says, or held.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// What the server does with a request.
pub enum Reply {
    /// Sends this response, then closes the connection.
    Send(Vec<u8>),
    /// Sends this response once [`FakeHttp::release`] has been called; until then the
    /// request hangs, and a client that gives up is counted ([`FakeHttp::abandoned`]).
    Held(Vec<u8>),
}

/// An HTTP/1.1 response with these headers and body, closing the connection.
pub fn response(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

#[derive(Default)]
struct State {
    heads: Vec<String>,
    abandoned: usize,
}

/// Answers every request on 127.0.0.1 with the [`Reply`] its closure gives for the path,
/// keeping the requests' heads. Runs on the tokio runtime it is started on.
pub struct FakeHttp {
    base: String,
    state: Arc<Mutex<State>>,
    released: watch::Sender<bool>,
}

impl FakeHttp {
    pub async fn start(answer: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            state: Arc::default(),
            released: watch::Sender::new(false),
        };
        let (state, released, answer) = (
            server.state.clone(),
            server.released.subscribe(),
            Arc::new(answer),
        );
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (state, mut released, answer) =
                    (state.clone(), released.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                    lock(&state).heads.push(head);
                    let response = match answer(&path) {
                        Reply::Send(response) => response,
                        Reply::Held(response) => {
                            let gone =
                                async { while socket.read(&mut byte).await.unwrap_or(0) > 0 {} };
                            tokio::select! {
                                _ = released.wait_for(|released| *released) => response,
                                () = gone => {
                                    lock(&state).abandoned += 1;
                                    return;
                                }
                            }
                        }
                    };
                    socket.write_all(&response).await.ok();
                    socket.shutdown().await.ok();
                });
            }
        });
        server
    }

    /// `path` on this server, as a URL.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The heads of the requests received, in order.
    pub fn heads(&self) -> Vec<String> {
        lock(&self.state).heads.clone()
    }

    pub fn requests(&self) -> usize {
        lock(&self.state).heads.len()
    }

    /// Held requests whose client closed the connection before they were answered.
    pub fn abandoned(&self) -> usize {
        lock(&self.state).abandoned
    }

    /// Answers the requests held, and those to come.
    pub fn release(&self) {
        self.released.send_replace(true);
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
