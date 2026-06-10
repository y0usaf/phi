//! Minimal single-purpose loopback HTTP responder for OAuth redirects.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

fn page(title: &str, detail: &str) -> String {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>phi</title></head>\
         <body style=\"font-family:system-ui;margin:4rem auto;max-width:32rem\">\
         <h2>{title}</h2><p>{detail}</p></body></html>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

pub struct CallbackServer {
    rx: mpsc::Receiver<String>,
    handle: JoinHandle<()>,
}

impl CallbackServer {
    /// Listens on `127.0.0.1:port` and resolves with the `code` query
    /// parameter of the first request to `path` whose `state` matches.
    pub async fn start(port: u16, path: &str, expected_state: String) -> std::io::Result<Self> {
        let host = std::env::var("PHI_OAUTH_CALLBACK_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let listener = TcpListener::bind((host.as_str(), port)).await?;
        let (tx, rx) = mpsc::channel(1);
        let path = path.to_owned();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 16 * 1024];
                let mut len = 0;
                while len < buf.len() {
                    match stream.read(&mut buf[len..]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            len += n;
                            if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let request = String::from_utf8_lossy(&buf[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
                    continue;
                };
                if url.path() != path {
                    let _ = stream
                        .write_all(page("Not found", "Callback route not found.").as_bytes())
                        .await;
                    continue;
                }
                let mut code = None;
                let mut state = None;
                let mut error = None;
                for (key, value) in url.query_pairs() {
                    match key.as_ref() {
                        "code" => code = Some(value.into_owned()),
                        "state" => state = Some(value.into_owned()),
                        "error" => error = Some(value.into_owned()),
                        _ => {}
                    }
                }
                if let Some(error) = error {
                    let _ = stream
                        .write_all(
                            page(
                                "Authentication failed",
                                &format!("The provider reported: {error}"),
                            )
                            .as_bytes(),
                        )
                        .await;
                    continue;
                }
                let (Some(code), Some(state)) = (code, state) else {
                    let _ = stream
                        .write_all(
                            page("Bad request", "Missing code or state parameter.").as_bytes(),
                        )
                        .await;
                    continue;
                };
                if state != expected_state {
                    let _ = stream
                        .write_all(page("Bad request", "State mismatch.").as_bytes())
                        .await;
                    continue;
                }
                let _ = stream
                    .write_all(
                        page(
                            "Login complete",
                            "Authentication completed. You can close this window.",
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = stream.shutdown().await;
                let _ = tx.send(code).await;
                return;
            }
        });
        Ok(Self { rx, handle })
    }

    /// Waits for the authorization code; `None` on cancellation.
    pub async fn wait(&mut self, cancel: &CancellationToken) -> Option<String> {
        tokio::select! {
            () = cancel.cancelled() => None,
            code = self.rx.recv() => code,
        }
    }
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
