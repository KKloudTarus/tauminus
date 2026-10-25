// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! AWS Signature Version 4 for S3, in the `Authorization` header with the
//! payload's SHA-256 signed. S3 encodes each path segment once and never
//! normalizes the path, unlike the other AWS services.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

/// A request to sign: what goes on the wire, minus the signature.
pub(crate) struct Request<'a> {
    pub method: &'a str,
    /// `host` or `host:port`, as the `Host` header carries it.
    pub host: &'a str,
    /// Path segments, unencoded: the bucket, then the key's parts.
    pub path: &'a [&'a str],
    /// Query pairs, unencoded; a bare flag has an empty value.
    pub query: &'a [(&'a str, &'a str)],
    /// Headers to sign besides `host`, `x-amz-content-sha256` and
    /// `x-amz-date`, with lowercase names.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload: &'a [u8],
}

/// The headers a signed request carries: `x-amz-date`,
/// `x-amz-content-sha256` and `authorization`.
pub(crate) struct Signed {
    pub date: String,
    pub payload_hash: String,
    pub authorization: String,
}

/// Unreserved characters pass; everything else is `%XX`, upper case.
pub(crate) fn encode(text: &str, out: &mut String) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

/// `/seg/seg`, each segment encoded once.
pub(crate) fn path(segments: &[&str]) -> String {
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        encode(segment, &mut out);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// The pairs sorted by name, then value, each encoded.
pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| {
            let (mut key, mut value) = (String::new(), String::new());
            encode(k, &mut key);
            encode(v, &mut value);
            (key, value)
        })
        .collect();
    encoded.sort();
    let mut out = String::new();
    for (i, (k, v)) in encoded.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        let _ = write!(out, "{k}={v}");
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn hmac(key: &[u8], data: &[u8]) -> Zeroizing<Vec<u8>> {
    // HMAC takes a key of any length.
    let mut mac = HmacSha256::new_from_slice(key).unwrap_or_else(|_| unreachable!());
    mac.update(data);
    Zeroizing::new(mac.finalize().into_bytes().to_vec())
}

/// `YYYYMMDDTHHMMSSZ` for `at`, UTC. Days to a civil date as in Howard
/// Hinnant's `civil_from_days`.
pub(crate) fn amz_date(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// Signs `request` for `region` with the key id and secret, at `date`
/// (`YYYYMMDDTHHMMSSZ`).
pub(crate) fn sign(
    request: &Request<'_>,
    region: &str,
    key_id: &str,
    secret: &str,
    date: &str,
) -> Signed {
    let payload_hash = hex(&Sha256::digest(request.payload));
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    headers.push(("host".into(), request.host.to_owned()));
    headers.push(("x-amz-content-sha256".into(), payload_hash.clone()));
    headers.push(("x-amz-date".into(), date.to_owned()));
    headers.sort();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let mut canonical = format!(
        "{}\n{}\n{}\n",
        request.method,
        path(request.path),
        query(request.query)
    );
    for (k, v) in &headers {
        let _ = writeln!(canonical, "{k}:{v}");
    }
    let _ = write!(canonical, "\n{signed_headers}\n{payload_hash}");
    let day = &date[..8];
    let scope = format!("{day}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = Zeroizing::new(format!("AWS4{secret}").into_bytes());
    for part in [day, region, "s3", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, to_sign.as_bytes()));
    Signed {
        date: date.to_owned(),
        payload_hash,
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The examples in AWS's "Signature Calculations for the Authorization
    /// Header: Transferring Payload in a Single Chunk".
    const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const DATE: &str = "20130524T000000Z";
    const HOST: &str = "examplebucket.s3.amazonaws.com";

    fn signature(signed: &Signed) -> &str {
        signed.authorization.rsplit("Signature=").next().unwrap()
    }

    #[test]
    fn the_get_object_example_signs_as_aws_shows() {
        let signed = sign(
            &Request {
                method: "GET",
                host: HOST,
                path: &["test.txt"],
                query: &[],
                headers: &[("range", "bytes=0-9")],
                payload: b"",
            },
            "us-east-1",
            KEY_ID,
            SECRET,
            DATE,
        );
        assert_eq!(
            signature(&signed),
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert!(signed.authorization.contains(
            "Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"
        ));
    }

    #[test]
    fn the_put_object_example_signs_as_aws_shows() {
        let signed = sign(
            &Request {
                method: "PUT",
                host: HOST,
                path: &["test$file.text"],
                query: &[],
                headers: &[
                    ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                    ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
                ],
                payload: b"Welcome to Amazon S3.",
            },
            "us-east-1",
            KEY_ID,
            SECRET,
            DATE,
        );
        assert_eq!(
            signature(&signed),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn the_list_objects_example_signs_as_aws_shows() {
        let signed = sign(
            &Request {
                method: "GET",
                host: HOST,
                path: &[],
                query: &[("max-keys", "2"), ("prefix", "J")],
                headers: &[],
                payload: b"",
            },
            "us-east-1",
            KEY_ID,
            SECRET,
            DATE,
        );
        assert_eq!(
            signature(&signed),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn segments_encode_once_and_queries_sort() {
        assert_eq!(path(&["b", "a b/ü+"]), "/b/a%20b%2F%C3%BC%2B");
        assert_eq!(path(&[]), "/");
        assert_eq!(
            query(&[
                ("prefix", "devices/"),
                ("list-type", "2"),
                ("continuation-token", "a=b+")
            ]),
            "continuation-token=a%3Db%2B&list-type=2&prefix=devices%2F"
        );
    }

    #[test]
    fn dates_are_utc_in_basic_form() {
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        let may_2013 = UNIX_EPOCH + Duration::from_hours(380_376);
        assert_eq!(amz_date(may_2013), DATE);
        let leap = UNIX_EPOCH + Duration::from_secs(951_825_599);
        assert_eq!(amz_date(leap), "20000229T115959Z");
    }
}
