// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;

use super::*;

/// A bucket in memory behind HTTP/1.1, one request per connection. With
/// `conditional` off it answers `If-None-Match` with 501, as a service
/// that does not take it. Lists two keys a page.
#[derive(Default)]
struct Bucket {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    seen: Mutex<Vec<(String, String, bool)>>,
    quirk: Quirk,
    /// Keys a listing page holds; 2 when 0.
    page: usize,
}

/// How the bucket strays from the plain service, one at a time.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Quirk {
    #[default]
    None,
    /// Answers `If-None-Match` with 501, as a service that does not take it.
    Unconditional,
    /// Refuses `If-None-Match` with 400 `NotImplemented`.
    Refuse400,
    /// Refuses every request with 403.
    Deny,
    /// Each page also lists the key the last one ended with.
    Duplicate,
    /// Reads a request body 8 KiB each 100 ms.
    Slow,
    /// Answers a `GET` of an object with a megabyte's length, then nothing.
    Hang,
}

/// What a request said.
struct Asked {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// Reads one request, the body `slow`ly if asked.
fn read_request(stream: &TcpStream, slow: bool) -> Asked {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map_or(0, |l| l.parse().unwrap());
    let mut body = vec![0; length];
    for piece in body.chunks_mut(if slow { 8 * 1024 } else { length.max(1) }) {
        reader.read_exact(piece).unwrap();
        if slow {
            std::thread::park_timeout(Duration::from_millis(100));
        }
    }
    Asked {
        method,
        target,
        headers,
        body,
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn respond(stream: &mut TcpStream, status: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
}

impl Bucket {
    fn serve(self: Arc<Self>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                self.answer(stream);
            }
        });
        format!("http://{address}")
    }

    /// A `ListObjectsV2` page of two keys, past the token.
    fn listing(
        objects: &BTreeMap<String, Vec<u8>>,
        query: &str,
        page: usize,
        duplicate: bool,
    ) -> String {
        let query: BTreeMap<String, String> = query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (decode(k), decode(v)))
            .collect();
        let prefix = query.get("prefix").cloned().unwrap_or_default();
        let after = query.get("continuation-token").cloned().unwrap_or_default();
        let keys: Vec<&String> = objects
            .keys()
            .filter(|k| k.starts_with(&prefix) && **k > after)
            .collect();
        let mut xml = String::from("<ListBucketResult>");
        if duplicate && !after.is_empty() {
            let _ = write!(xml, "<Contents><Key>{after}</Key></Contents>");
        }
        for k in keys.iter().take(page) {
            let _ = write!(
                xml,
                "<Contents><Key>{}</Key></Contents>",
                k.replace('&', "&amp;")
            );
        }
        if keys.len() > page {
            let _ = write!(
                xml,
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{}</NextContinuationToken>",
                keys[page - 1]
            );
        } else {
            xml.push_str("<IsTruncated>false</IsTruncated>");
        }
        xml.push_str("</ListBucketResult>");
        xml
    }

    fn answer(&self, mut stream: TcpStream) {
        let Asked {
            method,
            target,
            headers,
            body,
        } = read_request(&stream, self.quirk == Quirk::Slow);
        let signed = headers
            .get("authorization")
            .is_some_and(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=key/"));
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        self.seen
            .lock()
            .unwrap()
            .push((method.clone(), decode(path), signed));
        if self.quirk == Quirk::Deny {
            return respond(
                &mut stream,
                "403 Forbidden",
                b"<Error><Code>AccessDenied</Code></Error>",
            );
        }
        let key = decode(path).trim_start_matches("/bucket/").to_owned();
        let mut objects = self.objects.lock().unwrap();
        match method.as_str() {
            "GET" if path == "/bucket" => {
                let xml = Self::listing(
                    &objects,
                    query,
                    if self.page == 0 { 2 } else { self.page },
                    self.quirk == Quirk::Duplicate,
                );
                respond(&mut stream, "200 OK", xml.as_bytes());
            }
            "GET" if self.quirk == Quirk::Hang => {
                let head = "HTTP/1.1 200 OK\r\ncontent-length: 1048576\r\n\r\n";
                stream.write_all(head.as_bytes()).unwrap();
                drop(objects);
                std::thread::park_timeout(Duration::from_secs(5));
            }
            "GET" | "HEAD" => match objects.get(&key) {
                Some(bytes) if method == "GET" => respond(&mut stream, "200 OK", bytes),
                Some(_) => respond(&mut stream, "200 OK", b""),
                None => respond(
                    &mut stream,
                    "404 Not Found",
                    b"<Error><Code>NoSuchKey</Code></Error>",
                ),
            },
            "PUT" => {
                if headers.contains_key("if-none-match") {
                    if self.quirk == Quirk::Refuse400 {
                        return respond(
                            &mut stream,
                            "400 Bad Request",
                            b"<Error><Code>NotImplemented</Code></Error>",
                        );
                    }
                    if self.quirk == Quirk::Unconditional {
                        return respond(
                            &mut stream,
                            "501 Not Implemented",
                            b"<Error><Code>NotImplemented</Code></Error>",
                        );
                    }
                    if objects.contains_key(&key) {
                        return respond(
                            &mut stream,
                            "412 Precondition Failed",
                            b"<Error><Code>PreconditionFailed</Code></Error>",
                        );
                    }
                }
                objects.insert(key, body);
                respond(&mut stream, "200 OK", b"");
            }
            "DELETE" => {
                objects.remove(&key);
                respond(&mut stream, "204 No Content", b"");
            }
            _ => respond(&mut stream, "405 Method Not Allowed", b""),
        }
    }
}

fn config(endpoint: &str, prefix: &str) -> S3Config {
    S3Config {
        endpoint: endpoint.to_owned(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        prefix: prefix.into(),
        key_id: "key".into(),
        secret: Zeroizing::new("secret".into()),
    }
}

/// Runs `work` as the sync engine does: on a blocking thread.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(work)
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_go_in_and_out_under_the_folder_page_by_page() {
    let bucket = Arc::new(Bucket::default());
    let endpoint = Arc::clone(&bucket).serve();
    bucket
        .objects
        .lock()
        .unwrap()
        .insert("elsewhere/devices/x/1".into(), b"not ours".to_vec());
    let s3 = S3::new(&config(&format!("{endpoint}/"), "/team/sync/")).unwrap();
    blocking(move || {
        s3.put_new("key", b"wrapped").unwrap();
        let device = "0123456789abcdef0123456789abcdef";
        for seq in 1..=4 {
            s3.put_new(&crate::target::object_name(device, seq), &seq.to_le_bytes())
                .unwrap();
        }
        let mut listed = s3.list().unwrap();
        listed.sort();
        assert_eq!(listed.len(), 5, "{listed:?}");
        assert!(listed.contains(&"key".to_owned()));
        assert_eq!(s3.get("key", 64).unwrap(), b"wrapped");
        let first = crate::target::object_name(device, 1);
        let taken = s3.put_new(&first, b"again").unwrap_err();
        assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
        s3.remove(&first).unwrap();
        s3.remove(&first).unwrap();
        assert_eq!(
            s3.get(&first, 64).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let big = s3.get("key", 3).unwrap_err();
        assert_eq!(big.kind(), io::ErrorKind::InvalidData);
        assert!(s3.get("../escape", 64).is_err());
    });
    let seen = bucket.seen.lock().unwrap();
    assert!(seen.iter().all(|(_, _, signed)| *signed));
    assert!(
        seen.iter()
            .filter(|(m, _, _)| m != "GET" || !seen.is_empty())
            .all(|(_, path, _)| path.starts_with("/bucket")),
    );
    assert!(seen.iter().any(|(_, p, _)| p == "/bucket/team/sync/key"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_without_conditional_writes_is_asked_first() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Unconditional,
        ..Bucket::default()
    });
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, "")).unwrap();
    blocking(move || {
        s3.put_new("key", b"one").unwrap();
        let taken = s3.put_new("key", b"two").unwrap_err();
        assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(s3.get("key", 64).unwrap(), b"one");
    });
    let methods: Vec<String> = bucket
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _, _)| m.clone())
        .collect();
    // Tried once with the condition, then asked first each time.
    assert_eq!(methods, ["PUT", "HEAD", "PUT", "HEAD", "GET"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_names_its_code_and_the_secret_never_shows() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Deny,
        ..Bucket::default()
    });
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, "")).unwrap();
    assert!(!format!("{s3:?}").contains("secret"));
    assert!(!format!("{:?}", config(&endpoint, "")).contains("secret"));
    let error = blocking(move || s3.list().unwrap_err());
    // The key was refused: the target failed, the objects did not.
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "S3 list: 403 AccessDenied");
}

#[tokio::test(flavor = "multi_thread")]
async fn addresses_buckets_folders_and_regions_are_checked() {
    let ok = config("https://s3.example.com", "a/b-c_d.e");
    assert!(S3::new(&ok).is_ok());
    for (endpoint, bucket, prefix, region) in [
        ("http://s3.example.com", "bucket", "", "us-east-1"),
        ("https://s3.example.com/bucket", "bucket", "", "us-east-1"),
        ("https://user:pw@s3.example.com", "bucket", "", "us-east-1"),
        ("https://s3.example.com?x=1", "bucket", "", "us-east-1"),
        ("ftp://s3.example.com", "bucket", "", "us-east-1"),
        ("https://s3.example.com", "Bucket", "", "us-east-1"),
        ("https://s3.example.com", "b", "", "us-east-1"),
        ("https://s3.example.com", "bucket", "a//b", "us-east-1"),
        ("https://s3.example.com", "bucket", "a/../b", "us-east-1"),
        ("https://s3.example.com", "bucket", "a b", "us-east-1"),
        ("https://s3.example.com", "bucket", "", "US East"),
        ("https://s3.example.com", "bucket", "", ""),
    ] {
        let config = S3Config {
            bucket: bucket.into(),
            region: region.into(),
            ..config(endpoint, prefix)
        };
        assert!(
            S3::new(&config).is_err(),
            "{endpoint} {bucket} {prefix} {region}"
        );
    }
    assert!(S3::new(&config("http://127.0.0.1:9000", "")).is_ok());
    assert!(S3::new(&config("http://[::1]:9000", "")).is_ok());
    assert!(S3::new(&config("http://localhost:9000", "")).is_ok());
}

#[test]
fn listings_and_errors_parse() {
    let (keys, next) = list_page(
        b"<?xml version=\"1.0\"?><ListBucketResult><Name>b</Name><Contents><Key>a&amp;b</Key>\
          <Size>1</Size></Contents><Contents><Key>c</Key></Contents><IsTruncated>true</IsTruncated>\
          <NextContinuationToken>t+1</NextContinuationToken></ListBucketResult>",
    )
    .unwrap();
    assert_eq!(keys, ["a&b", "c"]);
    assert_eq!(next.as_deref(), Some("t+1"));
    let (_, last) =
        list_page(b"<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>")
            .unwrap();
    assert_eq!(last, None);
    assert!(
        list_page(b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>").is_err()
    );
    assert_eq!(
        error_code(b"<Error><Code>SignatureDoesNotMatch</Code><Message>m</Message></Error>")
            .as_deref(),
        Some("SignatureDoesNotMatch")
    );
    assert_eq!(error_code(b"not xml"), None);
    // Text the service chose is not shown.
    assert_eq!(
        error_code(b"<Error><Code>Re-enter your key at evil.example</Code></Error>"),
        None
    );
    let error = list_page(b"<ListBucketResult><Key>&evil;</Key></ListBucketResult>").unwrap_err();
    assert_eq!(error.to_string(), "S3 response: malformed XML");
}

/// Against a real service, when `TAUMINUS_S3_TEST` names one as
/// `endpoint bucket region key_id secret`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an S3 service"]
async fn a_real_service_takes_objects_once() {
    let spec = std::env::var("TAUMINUS_S3_TEST").expect("TAUMINUS_S3_TEST");
    let parts: Vec<&str> = spec.split_whitespace().collect();
    let config = S3Config {
        endpoint: parts[0].into(),
        bucket: parts[1].into(),
        region: parts[2].into(),
        prefix: format!("test-{}", std::process::id()),
        key_id: parts[3].into(),
        secret: Zeroizing::new(parts[4].into()),
    };
    let s3 = S3::new(&config).unwrap();
    blocking(move || {
        let device = "0123456789abcdef0123456789abcdef";
        let names: Vec<String> = (1..=3)
            .map(|seq| crate::target::object_name(device, seq))
            .collect();
        for name in &names {
            s3.put_new(name, name.as_bytes()).unwrap();
        }
        assert_eq!(
            s3.put_new(&names[0], b"x").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut listed = s3.list().unwrap();
        listed.sort();
        assert_eq!(listed, names);
        assert_eq!(s3.get(&names[2], 1024).unwrap(), names[2].as_bytes());
        for name in &names {
            s3.remove(name).unwrap();
        }
        assert_eq!(s3.list().unwrap(), Vec::<String>::new());
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn localhost_is_this_machine() {
    let bucket = Arc::new(Bucket::default());
    let endpoint = Arc::clone(&bucket)
        .serve()
        .replace("127.0.0.1", "localhost");
    let s3 = S3::new(&config(&endpoint, "")).unwrap();
    blocking(move || s3.put_new("key", b"k").unwrap());
    assert!(bucket.objects.lock().unwrap().contains_key("key"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_past_the_names_kept_is_refused() {
    let bucket = Arc::new(Bucket {
        page: 50_000,
        ..Bucket::default()
    });
    {
        let mut objects = bucket.objects.lock().unwrap();
        for seq in 1..=100_001 {
            objects.insert(
                crate::target::object_name("0123456789abcdef0123456789abcdef", seq),
                Vec::new(),
            );
        }
    }
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, "")).unwrap();
    let error = blocking(move || s3.list().unwrap_err());
    assert_eq!(error.to_string(), "S3 list: more than 100000 objects");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_upload_that_keeps_moving_arrives() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Slow,
        ..Bucket::default()
    });
    let endpoint = Arc::clone(&bucket).serve();
    // 160 KiB at about 80 KiB/s: past the 1 s of silence allowed, within
    // the time the payload is given at 32 KiB/s.
    let s3 = S3::new(&config(&endpoint, ""))
        .unwrap()
        .with_waits(Duration::from_secs(1), 32 * 1024);
    blocking(move || s3.put_new("key", &vec![7; 160 * 1024]).unwrap());
    assert_eq!(bucket.objects.lock().unwrap()["key"].len(), 160 * 1024);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_object_larger_than_it_may_be_is_not_read_and_a_stall_stops() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Hang,
        ..Bucket::default()
    });
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, ""))
        .unwrap()
        .with_waits(Duration::from_secs(1), 32 * 1024);
    let (big, stalled) = blocking(move || {
        (
            s3.get("key", 1024).unwrap_err(),
            s3.get("key", 2 * 1024 * 1024).unwrap_err(),
        )
    });
    // Refused by its declared length, before a byte of it.
    assert_eq!(big.kind(), io::ErrorKind::InvalidData, "{big}");
    assert_eq!(stalled.kind(), io::ErrorKind::TimedOut, "{stalled}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_400_not_implemented_also_means_ask_first() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Refuse400,
        ..Bucket::default()
    });
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, "")).unwrap();
    blocking(move || {
        s3.put_new("key", b"one").unwrap();
        assert_eq!(
            s3.put_new("key", b"two").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn only_sync_names_are_listed_once_each() {
    let bucket = Arc::new(Bucket {
        quirk: Quirk::Duplicate,
        ..Bucket::default()
    });
    {
        let mut objects = bucket.objects.lock().unwrap();
        for name in [
            "team/devices/foo",
            "team/keyring",
            "team/devices/0123456789abcdef0123456789abcdef/x",
        ] {
            objects.insert(name.into(), Vec::new());
        }
        for seq in 1..=5 {
            objects.insert(
                format!(
                    "team/{}",
                    crate::target::object_name("0123456789abcdef0123456789abcdef", seq)
                ),
                Vec::new(),
            );
        }
        objects.insert("team/key".into(), Vec::new());
    }
    let endpoint = Arc::clone(&bucket).serve();
    let s3 = S3::new(&config(&endpoint, "team")).unwrap();
    let listed = blocking(move || s3.list().unwrap());
    assert_eq!(listed.len(), 6, "{listed:?}");
    assert!(listed.iter().all(|n| valid_name(n)));
}

#[test]
fn an_error_code_past_64_characters_is_not_shown() {
    let long = format!("<Error><Code>{}</Code></Error>", "A".repeat(65));
    assert_eq!(error_code(long.as_bytes()), None);
    let ok = format!("<Error><Code>{}</Code></Error>", "A".repeat(64));
    assert!(error_code(ok.as_bytes()).is_some());
}
