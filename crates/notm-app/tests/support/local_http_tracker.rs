use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct LocalHttpTracker {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LocalHttpTracker {
    pub(crate) fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("binding the local HTTP request tracker")?;
        listener
            .set_nonblocking(true)
            .context("configuring the local HTTP request tracker")?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("notm-local-http-tracker".to_string())
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = handle_request(stream, address, &worker_requests) {
                                worker_requests
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .push(format!("<tracker-error:{error}>"));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(POLL_INTERVAL);
                        }
                        Err(error) => {
                            worker_requests
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(format!("<accept-error:{error}>"));
                            break;
                        }
                    }
                }
            })
            .context("starting the local HTTP request tracker")?;

        Ok(Self {
            address,
            requests,
            shutdown,
            worker: Some(worker),
        })
    }

    pub(crate) fn url(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("http://{}{path}", self.address)
    }

    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn wait_for_requests(
        &self,
        expected: &[&str],
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let requests = self.requests();
            if requests == expected {
                return Ok(());
            }
            ensure!(
                requests.len() <= expected.len()
                    && requests
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| actual == expected),
                "local HTTP tracker saw unexpected requests: expected={expected:?}, actual={requests:?}"
            );
            ensure!(
                Instant::now() < deadline,
                "local HTTP tracker did not see expected requests within {timeout:?}: expected={expected:?}, actual={requests:?}"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub(crate) fn ensure_stable(
        &self,
        expected: &[&str],
        duration: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            let requests = self.requests();
            ensure!(
                requests == expected,
                "local HTTP tracker request set changed: expected={expected:?}, actual={requests:?}"
            );
            if Instant::now() >= deadline {
                return Ok(());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for LocalHttpTracker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_request(
    mut stream: TcpStream,
    address: SocketAddr,
    requests: &Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    while bytes.len() < 16 * 1024 {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&bytes);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("HTTP tracker received a request without a path")?
        .to_string();
    requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(path.clone());

    if path == "/redirect" {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: http://{address}/redirect-target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
    } else if path.ends_with(".css") {
        let body = format!("body{{background-image:url(http://{address}/css-nested)}}");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
    } else if path.ends_with(".html") {
        let body = format!(
            "<html><body><img src=\"http://{address}/nested-document-image\"></body></html>"
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
    } else {
        // A complete one-pixel transparent GIF keeps WebKit's resource load
        // deterministic without reaching any network service outside this
        // process.
        const PIXEL: &[u8] = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            PIXEL.len()
        )?;
        stream.write_all(PIXEL)?;
    }
    stream.flush()?;
    Ok(())
}
