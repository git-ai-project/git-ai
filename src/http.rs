use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Response {
    pub status_code: u16,
    body: Vec<u8>,
}

impl Response {
    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.body
    }
}

pub struct Request {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    timeout: Option<Duration>,
}

impl Request {
    pub fn get(url: &str, timeout_secs: Option<u64>) -> Self {
        Self {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: vec![
                ("User-Agent".to_string(), format!("git-ai/{}", env!("CARGO_PKG_VERSION"))),
            ],
            timeout: timeout_secs.map(Duration::from_secs),
        }
    }

    pub fn post(url: &str, timeout_secs: Option<u64>) -> Self {
        Self {
            method: "POST".to_string(),
            url: url.to_string(),
            headers: vec![
                ("User-Agent".to_string(), format!("git-ai/{}", env!("CARGO_PKG_VERSION"))),
            ],
            timeout: timeout_secs.map(Duration::from_secs),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn send(self) -> Result<Response, String> {
        self.send_body(None)
    }

    pub fn send_string(self, body: &str) -> Result<Response, String> {
        self.send_body(Some(body.as_bytes()))
    }

    fn send_body(self, body: Option<&[u8]>) -> Result<Response, String> {
        let (scheme, host, port, path) = parse_url(&self.url)?;
        let use_tls = scheme == "https";

        let addr_str = format!("{}:{}", host, port);
        let tcp = if let Some(timeout) = self.timeout {
            use std::net::ToSocketAddrs;
            let socket_addr = addr_str
                .to_socket_addrs()
                .map_err(|e| format!("DNS resolution for '{}' failed: {}", host, e))?
                .next()
                .ok_or_else(|| format!("No addresses found for '{}'", host))?;
            TcpStream::connect_timeout(&socket_addr, timeout)
                .map_err(|e| format!("Connection to {} failed: {}", addr_str, e))?
        } else {
            TcpStream::connect(&addr_str)
                .map_err(|e| format!("Connection to {} failed: {}", addr_str, e))?
        };

        if let Some(timeout) = self.timeout {
            tcp.set_read_timeout(Some(timeout)).ok();
            tcp.set_write_timeout(Some(timeout)).ok();
        }

        let mut request_bytes = Vec::new();
        write!(request_bytes, "{} {} HTTP/1.1\r\n", self.method, path)
            .map_err(|e| e.to_string())?;
        write!(request_bytes, "Host: {}\r\n", host).map_err(|e| e.to_string())?;
        write!(request_bytes, "Connection: close\r\n").map_err(|e| e.to_string())?;

        for (name, value) in &self.headers {
            write!(request_bytes, "{}: {}\r\n", name, value).map_err(|e| e.to_string())?;
        }

        if let Some(b) = body {
            write!(request_bytes, "Content-Length: {}\r\n", b.len()).map_err(|e| e.to_string())?;
        }

        write!(request_bytes, "\r\n").map_err(|e| e.to_string())?;
        if let Some(b) = body {
            request_bytes.extend_from_slice(b);
        }

        if use_tls {
            let connector =
                native_tls::TlsConnector::new().map_err(|e| format!("TLS init failed: {}", e))?;
            let mut stream = connector
                .connect(&host, tcp)
                .map_err(|e| format!("TLS handshake with {} failed: {}", host, e))?;
            stream
                .write_all(&request_bytes)
                .map_err(|e| format!("Write failed: {}", e))?;
            stream.flush().map_err(|e| format!("Flush failed: {}", e))?;
            read_response(&mut stream)
        } else {
            let mut stream = tcp;
            stream
                .write_all(&request_bytes)
                .map_err(|e| format!("Write failed: {}", e))?;
            stream.flush().map_err(|e| format!("Flush failed: {}", e))?;
            read_response(&mut stream)
        }
    }
}

fn read_response(stream: &mut impl Read) -> Result<Response, String> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    loop {
        let n = stream.read(&mut tmp).map_err(|e| format!("Read failed: {}", e))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    let header_end = find_header_end(&buf).ok_or("Malformed HTTP response: no header terminator")?;
    let header_str =
        std::str::from_utf8(&buf[..header_end]).map_err(|_| "Non-UTF8 HTTP headers")?;

    let status_code = parse_status_line(header_str)?;
    let body = extract_body(&buf, header_end, header_str);

    Ok(Response { status_code, body })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

fn parse_status_line(headers: &str) -> Result<u16, String> {
    let first_line = headers.lines().next().ok_or("Empty response")?;
    let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(format!("Invalid status line: {}", first_line));
    }
    parts[1]
        .parse()
        .map_err(|_| format!("Invalid status code: {}", parts[1]))
}

fn extract_body(buf: &[u8], header_end: usize, headers: &str) -> Vec<u8> {
    let raw_body = &buf[header_end..];

    if is_chunked(headers) {
        decode_chunked(raw_body)
    } else {
        raw_body.to_vec()
    }
}

fn is_chunked(headers: &str) -> bool {
    headers
        .lines()
        .any(|line| {
            line.to_ascii_lowercase()
                .starts_with("transfer-encoding:")
                && line.to_ascii_lowercase().contains("chunked")
        })
}

fn decode_chunked(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut pos = 0;

    loop {
        let line_end = match data[pos..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => pos + p,
            None => break,
        };

        let size_str = match std::str::from_utf8(&data[pos..line_end]) {
            Ok(s) => s.trim(),
            Err(_) => break,
        };

        let chunk_size = match usize::from_str_radix(size_str, 16) {
            Ok(s) => s,
            Err(_) => break,
        };

        if chunk_size == 0 {
            break;
        }

        let chunk_start = line_end + 2;
        let chunk_end = chunk_start + chunk_size;

        if chunk_end > data.len() {
            result.extend_from_slice(&data[chunk_start..]);
            break;
        }

        result.extend_from_slice(&data[chunk_start..chunk_end]);
        pos = chunk_end + 2;
    }

    result
}

fn parse_url(url: &str) -> Result<(String, String, u16, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("No scheme in URL: {}", url))?;

    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return Err(format!("Unsupported scheme: {}", scheme)),
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    // Strip userinfo (user:pass@)
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };

    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let port_str = &host_port[i + 1..];
            match port_str.parse::<u16>() {
                Ok(p) => (&host_port[..i], p),
                Err(_) => (host_port, default_port),
            }
        }
        None => (host_port, default_port),
    };

    Ok((
        scheme.to_string(),
        host.to_string(),
        port,
        path.to_string(),
    ))
}
