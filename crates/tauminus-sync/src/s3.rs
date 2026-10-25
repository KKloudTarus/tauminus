// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! S3-compatible storage as a sync target: AWS S3, Cloudflare R2,
//! Backblaze B2, `MinIO` and the like, through path-style requests signed
//! with `SigV4` in the header. Objects are written once under names only
//! this device uses, so no write needs to be conditional (ADR 0030); a
//! new one is still sent with `If-None-Match: *`, and where the service
//! does not take it, a `HEAD` goes first.

mod sign;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use quick_xml::events::Event;
use reqwest::{Client, Method, StatusCode, Url, redirect};
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::runtime::Handle;
use zeroize::Zeroizing;

use crate::target::{Target, valid_name};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Silence allowed before a response starts and between two reads of its
/// body: a large object on a slow link takes as long as it takes, a
/// stalled one stops.
const IDLE: Duration = Duration::from_secs(60);
/// The slowest upload waited for, in bytes a second: 128 kbit/s. Sending
/// a payload may take its size over this, on top of [`IDLE`].
const SLOWEST: u64 = 16 * 1024;
/// Largest listing page read; a thousand keys take about 300 KB.
const MAX_PAGE: u64 = 8 * 1024 * 1024;
/// Most pages listed: 64 000 objects, far past what sync writes.
const MAX_PAGES: usize = 64;
/// Most names a listing keeps: a service packing pages with names does
/// not fill memory.
const MAX_NAMES: usize = 100_000;
/// Largest error body read for its code.
const MAX_ERROR: u64 = 64 * 1024;

/// Where the objects go, and the key that may write them.
#[derive(Clone)]
pub struct S3Config {
    /// `https://host[:port]`; `http` only for this machine, as a local
    /// `MinIO` is.
    pub endpoint: String,
    /// `us-east-1`, or `auto` for R2.
    pub region: String,
    pub bucket: String,
    /// Folders inside the bucket, `a/b`; empty for its top.
    pub prefix: String,
    pub key_id: String,
    pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_owned())
}

/// A bucket name as S3 allows one: 3 to 63 of `a-z 0-9 . -`.
fn valid_bucket(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// Folders of `A-Z a-z 0-9 . _ -`, none empty or a dot name: nothing
/// that encodes in more than one way.
fn valid_prefix(prefix: &str) -> bool {
    prefix.is_empty()
        || prefix.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.len() <= 64
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// The text of each element whose path from the root is wanted, with
/// entity and character references resolved. O(n) in the body.
fn texts(body: &[u8], wanted: &[&[&str]]) -> io::Result<Vec<(usize, String)>> {
    // The service's own words stay out of the message: it may write
    // anything there.
    let bad = |_: &dyn std::fmt::Display| {
        io::Error::new(io::ErrorKind::InvalidData, "S3 response: malformed XML")
    };
    let mut reader = quick_xml::Reader::from_reader(body);
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event().map_err(|e| bad(&e))? {
            Event::Start(tag) => {
                path.push(tag.name().as_ref().to_owned());
                text.clear();
            }
            Event::Text(part) => text.push_str(&part.xml10_content()),
            Event::GeneralRef(entity) => {
                let name = entity.xml10_content();
                let resolved = match name.strip_prefix('#') {
                    Some(code) => {
                        let value = match code.strip_prefix('x') {
                            Some(hex) => u32::from_str_radix(hex, 16),
                            None => code.parse(),
                        };
                        value.ok().and_then(char::from_u32).map(String::from)
                    }
                    None => quick_xml::escape::resolve_xml_entity(&name).map(str::to_owned),
                };
                text.push_str(&resolved.ok_or_else(|| bad(&"unknown entity"))?);
            }
            Event::End(_) => {
                if let Some(at) = wanted.iter().position(|w| w.iter().eq(path.iter())) {
                    out.push((at, std::mem::take(&mut text)));
                }
                path.pop();
                text.clear();
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

/// The service's error code from an error body, `Code` in `Error`, when
/// it is one: 1 to 64 letters and digits, as S3's are. Anything else the
/// service wrote is not shown.
pub(crate) fn error_code(body: &[u8]) -> Option<String> {
    texts(body, &[&["Error", "Code"]])
        .ok()?
        .into_iter()
        .next()
        .map(|(_, code)| code)
        .filter(|code| {
            (1..=64).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// One `ListObjectsV2` page: its keys, and the token for the next page
/// when it is truncated.
pub(crate) fn list_page(body: &[u8]) -> io::Result<(Vec<String>, Option<String>)> {
    let found = texts(
        body,
        &[
            &["ListBucketResult", "Contents", "Key"],
            &["ListBucketResult", "IsTruncated"],
            &["ListBucketResult", "NextContinuationToken"],
        ],
    )?;
    let mut keys = Vec::new();
    let mut truncated = false;
    let mut token = None;
    for (at, text) in found {
        match at {
            0 => keys.push(text),
            1 => truncated = text == "true",
            _ => token = Some(text),
        }
    }
    if truncated && token.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "S3 list: truncated with no continuation token",
        ));
    }
    Ok((keys, if truncated { token } else { None }))
}

/// A bucket, or a folder in one, as a sync target.
pub struct S3 {
    /// `scheme://host[:port]`.
    origin: String,
    /// The `Host` header: `host[:port]`.
    host: String,
    region: String,
    bucket: String,
    /// Folders, each a path segment.
    prefix: Vec<String>,
    key_id: String,
    secret: Zeroizing<String>,
    client: Client,
    runtime: Handle,
    /// [`IDLE`] and [`SLOWEST`]; a test shortens them.
    idle: Duration,
    slowest: u64,
    /// Whether the service takes `If-None-Match: *` on a `PUT`.
    conditional: AtomicBool,
}

impl std::fmt::Debug for S3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3")
            .field("origin", &self.origin)
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl S3 {
    /// Checks `config` and makes the client; nothing is sent yet. Runs its
    /// requests on the Tokio runtime current here.
    ///
    /// # Errors
    /// An address that is not `https` (or `http` to this machine), a bad
    /// bucket, folder or region, no runtime, or no certificate store.
    pub fn new(config: &S3Config) -> io::Result<Self> {
        let url = Url::parse(config.endpoint.trim()).map_err(|_| invalid("not an S3 address"))?;
        let host = url
            .host_str()
            .ok_or_else(|| invalid("an S3 address needs a host"))?;
        let local = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
            || host == "localhost";
        match url.scheme() {
            "https" => {}
            "http" if local => {}
            _ => return Err(invalid("an S3 address is https, or http to this machine")),
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(invalid(
                "an S3 address is only its scheme, host and port; the bucket goes apart",
            ));
        }
        let bucket = config.bucket.trim();
        if !valid_bucket(bucket) {
            return Err(invalid("a bucket name is 3 to 63 of a-z, 0-9, '.' and '-'"));
        }
        let prefix = config.prefix.trim().trim_matches('/');
        if !valid_prefix(prefix) {
            return Err(invalid(
                "a folder in the bucket is made of A-Z, a-z, 0-9, '.', '_' and '-', between '/'",
            ));
        }
        let region = config.region.trim();
        if region.is_empty()
            || region.len() > 32
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid("a region is like us-east-1, or auto"));
        }
        let key_id = config.key_id.trim();
        if key_id.is_empty() || !key_id.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid("an access key id is printable ASCII"));
        }
        let runtime =
            Handle::try_current().map_err(|_| io::Error::other("no runtime for S3 requests"))?;
        let host_header = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        Ok(Self {
            origin: format!("{}://{host_header}", url.scheme()),
            host: host_header,
            region: region.to_owned(),
            bucket: bucket.to_owned(),
            prefix: prefix
                .split('/')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect(),
            key_id: key_id.to_owned(),
            secret: config.secret.clone(),
            client: client(
                (host == "localhost").then(|| url.port_or_known_default().unwrap_or(80)),
            )?,
            runtime,
            conditional: AtomicBool::new(true),
            idle: IDLE,
            slowest: SLOWEST,
        })
    }

    /// Shorter waits, for tests.
    #[cfg(test)]
    fn with_waits(mut self, idle: Duration, slowest: u64) -> Self {
        self.idle = idle;
        self.slowest = slowest;
        self
    }

    /// The prefix as listed: `a/b/`, or empty.
    fn list_prefix(&self) -> String {
        self.prefix.iter().fold(String::new(), |mut out, part| {
            out.push_str(part);
            out.push('/');
            out
        })
    }

    /// The sync names under `under`, past the folder, page by page.
    fn list_under(&self, under: &str) -> io::Result<Vec<String>> {
        let prefix = self.list_prefix();
        let wanted = format!("{prefix}{under}");
        let mut names = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut query = vec![("list-type", "2"), ("prefix", wanted.as_str())];
            if let Some(token) = &token {
                query.push(("continuation-token", token));
            }
            let (status, length, body) =
                self.send(&Method::GET, None, &query, &[], b"", MAX_PAGE)?;
            if !status.is_success() {
                return Err(failure("list", status, &body));
            }
            if length.is_some_and(|l| l > MAX_PAGE) || body.len() as u64 > MAX_PAGE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "S3 list: page too large",
                ));
            }
            let (keys, next) = list_page(&body)?;
            names.extend(
                keys.iter()
                    .filter_map(|k| k.strip_prefix(&prefix))
                    .filter(|k| valid_name(k))
                    .map(str::to_owned),
            );
            if names.len() > MAX_NAMES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("S3 list: more than {MAX_NAMES} objects"),
                ));
            }
            match next {
                Some(next) => token = Some(next),
                None => return Ok(names),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("S3 list: more than {MAX_PAGES} pages"),
        ))
    }

    /// Sends a signed request for object `name` (or the bucket), and
    /// returns its status and up to `limit` bytes of its body.
    fn send(
        &self,
        method: &Method,
        name: Option<&str>,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
        body: &[u8],
        limit: u64,
    ) -> io::Result<(StatusCode, Option<u64>, Vec<u8>)> {
        // The bucket, then for an object the folders and its name; a
        // listing gives the folders in its query.
        let mut segments: Vec<&str> = vec![&self.bucket];
        if let Some(name) = name {
            segments.extend(self.prefix.iter().map(String::as_str));
            segments.extend(name.split('/'));
        }
        let mut url = format!("{}{}", self.origin, sign::path(&segments));
        let encoded = sign::query(query);
        if !encoded.is_empty() {
            url.push('?');
            url.push_str(&encoded);
        }
        let signed = sign::sign(
            &sign::Request {
                method: method.as_str(),
                host: &self.host,
                path: &segments,
                query,
                headers,
                payload: body,
            },
            &self.region,
            &self.key_id,
            &self.secret,
            &sign::amz_date(SystemTime::now()),
        );
        let mut request = self
            .client
            .request(method.clone(), url)
            .header("x-amz-date", &signed.date)
            .header("x-amz-content-sha256", &signed.payload_hash)
            .header("authorization", &signed.authorization)
            .body(body.to_vec());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let failed = |e: reqwest::Error| {
            let what = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "could not connect"
            } else {
                "the request failed"
            };
            io::Error::other(format!("S3: {what}: {}", e.without_url()))
        };
        let stalled =
            || io::Error::new(io::ErrorKind::TimedOut, "S3: the service stopped answering");
        // Sending takes as long as the payload needs at the slowest rate
        // waited for; reading, each chunk within the idle time.
        let sending = self.idle + Duration::from_secs(body.len() as u64 / self.slowest);
        self.runtime.block_on(async {
            let mut response = tokio::time::timeout(sending, request.send())
                .await
                .map_err(|_| stalled())?
                .map_err(failed)?;
            let status = response.status();
            let length = response.content_length();
            let limit = if status.is_success() {
                limit
            } else {
                MAX_ERROR
            };
            // Larger than allowed by its own word: not read at all.
            if length.is_some_and(|l| l > limit) {
                return Ok((status, length, Vec::new()));
            }
            let size = length.unwrap_or(0).min(limit);
            let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            while let Some(chunk) = tokio::time::timeout(self.idle, response.chunk())
                .await
                .map_err(|_| stalled())?
                .map_err(failed)?
            {
                out.extend_from_slice(&chunk);
                if out.len() as u64 > limit {
                    break;
                }
            }
            Ok((status, length, out))
        })
    }
}

/// The failure a response's status and body tell, for `what`.
fn failure(what: &str, status: StatusCode, body: &[u8]) -> io::Error {
    let code = error_code(body).unwrap_or_default();
    let kind = match status {
        StatusCode::NOT_FOUND => io::ErrorKind::NotFound,
        StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => io::ErrorKind::AlreadyExists,
        // A refused key or bucket policy (401, 403) included: a failure of
        // the target, not of the object, which the engine would take for
        // a forgery.
        _ => io::ErrorKind::Other,
    };
    io::Error::new(
        kind,
        format!("S3 {what}: {} {code}", status.as_u16())
            .trim_end()
            .to_owned(),
    )
}

/// HTTPS with the OS's certificates, no proxy, no redirects. `localhost`
/// on `port` goes to 127.0.0.1 whatever name resolution says, as the
/// plain `http` it may use is allowed for this machine only.
fn client(localhost: Option<u16>) -> io::Result<Client> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(BuilderVerifierExt::with_platform_verifier)
        .map_err(|e| io::Error::other(format!("TLS: {e}")))?
        .with_no_client_auth();
    let mut builder = Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT);
    if let Some(port) = localhost {
        builder = builder.resolve(
            "localhost",
            std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), port),
        );
    }
    builder
        .build()
        .map_err(|e| io::Error::other(format!("S3 client: {}", e.without_url())))
}

fn checked(name: &str) -> io::Result<&str> {
    if valid_name(name) {
        Ok(name)
    } else {
        Err(invalid("not a sync object's name"))
    }
}

impl Target for S3 {
    fn refresh(&self) -> io::Result<()> {
        Ok(())
    }

    /// Names past the prefix that a sync object may have; anything else
    /// in the bucket is not this target's. O(n) over at most 64 pages.
    fn list(&self) -> io::Result<Vec<String>> {
        // Only where sync writes: other data in the bucket costs nothing.
        let mut names = self.list_under("devices/")?;
        names.extend(self.list_under(crate::target::KEY)?);
        // A service listing a name twice lists it once here.
        names.sort_unstable();
        names.dedup();
        Ok(names)
    }

    fn get(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let name = checked(name)?;
        let (status, length, body) = self.send(&Method::GET, Some(name), &[], &[], b"", limit)?;
        if !status.is_success() {
            return Err(failure("get", status, &body));
        }
        if length.is_some_and(|l| l > limit) || body.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "S3 get: object larger than allowed",
            ));
        }
        Ok(body)
    }

    fn put_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let name = checked(name)?;
        if self.conditional.load(Ordering::Relaxed) {
            let (status, _, body) = self.send(
                &Method::PUT,
                Some(name),
                &[],
                &[("if-none-match", "*")],
                bytes,
                0,
            )?;
            if status.is_success() {
                return Ok(());
            }
            let code = error_code(&body);
            let unsupported = status == StatusCode::NOT_IMPLEMENTED
                || (status == StatusCode::BAD_REQUEST && code.as_deref() == Some("NotImplemented"));
            if !unsupported {
                return Err(failure("put", status, &body));
            }
            // The service writes only unconditionally: ask first.
            self.conditional.store(false, Ordering::Relaxed);
        }
        let (status, _, body) = self.send(&Method::HEAD, Some(name), &[], &[], b"", 0)?;
        match status {
            s if s.is_success() => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "S3 put: name taken",
                ));
            }
            StatusCode::NOT_FOUND => {}
            s => return Err(failure("put", s, &body)),
        }
        let (status, _, body) = self.send(&Method::PUT, Some(name), &[], &[], bytes, 0)?;
        if status.is_success() {
            Ok(())
        } else {
            Err(failure("put", status, &body))
        }
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        let name = checked(name)?;
        let (status, _, body) = self.send(&Method::DELETE, Some(name), &[], &[], b"", 0)?;
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(failure("remove", status, &body))
        }
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
