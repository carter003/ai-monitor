//! Embedded HTTP service: all workers belong to the caller's process.
use super::{self as web, Query};
use crate::plans::{self, PlanOptions};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Dropping this owner stops accepting, cancels active socket I/O and joins workers.
/// No child process is spawned; process exit (including SIGKILL) closes every socket.
pub struct Server {
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    active: Vec<Arc<Mutex<Option<TcpStream>>>>,
    workers: Vec<JoinHandle<()>>,
}
impl Server {
    pub fn start(database: PathBuf, port: u16, plans: PlanOptions) -> io::Result<Self> {
        let plans = Arc::new(plans);
        let listener = Arc::new(TcpListener::bind((Ipv4Addr::LOCALHOST, port))?);
        listener.set_nonblocking(true)?;
        let mut server = Self {
            address: listener.local_addr()?,
            stopped: Arc::new(AtomicBool::new(false)),
            active: Vec::new(),
            workers: Vec::new(),
        };
        for index in 0..4 {
            let listener = Arc::clone(&listener);
            let stopped = Arc::clone(&server.stopped);
            let database = database.clone();
            let plans = Arc::clone(&plans);
            let active = Arc::new(Mutex::new(None));
            server.active.push(Arc::clone(&active));
            let worker = thread::Builder::new()
                .name(format!("usage-web-{index}"))
                .spawn(move || {
                    while !stopped.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                {
                                    let mut slot = active.lock().unwrap_or_else(|e| e.into_inner());
                                    if stopped.load(Ordering::Acquire) {
                                        break;
                                    }
                                    let Ok(socket) = stream.try_clone() else {
                                        continue;
                                    };
                                    *slot = Some(socket);
                                }
                                // A browser disconnect is routine. Do not write into the TUI.
                                let _ = handle(stream, &database, &plans);
                                active.lock().unwrap_or_else(|e| e.into_inner()).take();
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(25));
                            }
                            Err(_) => break,
                        }
                    }
                })?;
            server.workers.push(worker);
        }
        Ok(server)
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        for active in &self.active {
            if let Some(socket) = active.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8]) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'\r\n\r\n", body.len())?;
    stream.write_all(body)
}
fn handle(mut stream: TcpStream, database: &Path, plans: &PlanOptions) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    let mut reader = BufReader::new((&stream).take(16385));
    reader.read_line(&mut line)?;
    if line.len() > 8192 {
        return respond(
            &mut stream,
            "414 URI Too Long",
            "text/plain",
            b"URI too long",
        );
    }
    let mut header_size = line.len();
    loop {
        let mut header = String::new();
        let count = reader.read_line(&mut header)?;
        header_size += count;
        if count == 0 || header_size > 16384 {
            drop(reader);
            return respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                b"Invalid headers",
            );
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
    }
    drop(reader);
    let mut parts = line.split_whitespace();
    if parts.next() != Some("GET") {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET only",
        );
    }
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let asset: Option<(&str, &[u8])> = match path {
        "/" => Some(("text/html; charset=utf-8", include_bytes!("index.html"))),
        "/query" => Some(("text/html; charset=utf-8", include_bytes!("query.html"))),
        "/plans" => Some(("text/html; charset=utf-8", include_bytes!("plans.html"))),
        "/app.js" => Some(("text/javascript; charset=utf-8", include_bytes!("app.js"))),
        "/style.css" => Some(("text/css; charset=utf-8", include_bytes!("style.css"))),
        _ => None,
    };
    if let Some((mime, body)) = asset {
        return respond(&mut stream, "200 OK", mime, body);
    }
    if path == "/api/plans" {
        return match plans::report(database, plans) {
            Ok(report) => respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                &serde_json::to_vec(&report)?,
            ),
            Err(error) => respond(
                &mut stream,
                "400 Bad Request",
                "application/json; charset=utf-8",
                serde_json::json!({"error": error}).to_string().as_bytes(),
            ),
        };
    }
    if path != "/api/usage" {
        return respond(&mut stream, "404 Not Found", "text/plain", b"Not found");
    }
    let query = match Query::parse(query) {
        Ok(query) => query,
        Err(error) => {
            return respond(
                &mut stream,
                "400 Bad Request",
                "application/json; charset=utf-8",
                serde_json::json!({"error": error}).to_string().as_bytes(),
            )
        }
    };
    match web::load(database, query) {
        Ok(report) => respond(
            &mut stream,
            "200 OK",
            "application/json; charset=utf-8",
            &serde_json::to_vec(&report)?,
        ),
        Err(error) => respond(
            &mut stream,
            "400 Bad Request",
            "application/json; charset=utf-8",
            serde_json::json!({"error": error}).to_string().as_bytes(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_server_cancels_partial_requests_and_releases_listener() {
        let server = Server::start(PathBuf::from("/unused.db"), 0, PlanOptions::default()).unwrap();
        let address = server.address();
        let mut clients: Vec<_> = (0..4)
            .map(|_| {
                let mut stream = TcpStream::connect(address).unwrap();
                stream.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
                stream
            })
            .collect();
        thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(TcpStream::connect(address).is_err());
        for client in &mut clients {
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut response = Vec::new();
            let result = client.read_to_end(&mut response);
            assert!(result.is_ok() || result.unwrap_err().kind() == io::ErrorKind::ConnectionReset);
        }
        let _listener = TcpListener::bind(address).unwrap();
    }

    #[test]
    fn a_port_conflict_does_not_replace_the_existing_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let error = Server::start(PathBuf::from("/unused.db"), address.port(), PlanOptions::default())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(TcpStream::connect(address).is_ok());
    }
}
