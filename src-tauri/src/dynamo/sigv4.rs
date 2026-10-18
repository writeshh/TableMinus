//! AWS Signature Version 4, for the `DynamoDB` API: a canonical request, a string to sign and an HMAC-SHA256 chain
//! from the secret key. Small enough to keep here rather than pull in the AWS SDK.

use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const BLOCK: usize = 64;

pub fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.map(|x| x ^ b);
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(data).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |s, b| s + &format!("{b:02x}"))
}

/// The time as `20150830T123600Z`, and its date `20150830`.
pub fn amz_date(at: SystemTime) -> (String, String) {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let date = format!("{year:04}{month:02}{day:02}");
    (format!("{date}T{:02}{:02}{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60), date)
}

/// What one request is signed over.
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Already in canonical form (sorted, URI-encoded); empty for none.
    pub query: &'a str,
    /// Lower-case names with trimmed values, `host` and `x-amz-date` among them.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

/// The `Authorization` header for `r`, signed for `service` in `region` at `amz_date` (with `date` its day).
pub fn authorization(
    r: &Request,
    access_key: &str,
    secret: &str,
    region: &str,
    service: &str,
    (amz_date, date): (&str, &str),
) -> String {
    let mut headers: Vec<&(&str, &str)> = r.headers.iter().collect();
    headers.sort_by_key(|(name, _)| *name);
    let canonical_headers = headers.iter().fold(String::new(), |mut s, (n, v)| {
        let _ = writeln!(s, "{n}:{v}"); // writing to a String can't fail
        s
    });
    let signed: Vec<&str> = headers.iter().map(|(n, _)| *n).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}",
        r.method,
        r.path,
        r.query,
        hex(&Sha256::digest(r.body))
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex(&Sha256::digest(canonical.as_bytes())));
    let key = [region, service, "aws4_request"]
        .iter()
        .fold(hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes()), |k, part| hmac(&k, part.as_bytes()));
    let signature = hex(&hmac(&key, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}")
}

#[cfg(test)]
mod tests {
    use super::{Request, amz_date, authorization, hex, hmac};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn hmac_should_match_rfc_4231_test_case_2() {
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn amz_date_should_write_the_time_in_utc() {
        let at = UNIX_EPOCH + Duration::from_mins(24_015_636); // 2015-08-30 12:36:00 UTC
        assert_eq!(amz_date(at), ("20150830T123600Z".into(), "20150830".into()));
        assert_eq!(amz_date(UNIX_EPOCH + Duration::from_hours(264_384)).1, "20000229");
    }

    /// The example in AWS's "Create a signed AWS API request" (IAM `ListUsers`).
    #[test]
    fn authorization_should_match_aws_documented_example() {
        let r = Request {
            method: "GET",
            path: "/",
            query: "Action=ListUsers&Version=2010-05-08",
            headers: &[
                ("content-type", "application/x-www-form-urlencoded; charset=utf-8"),
                ("host", "iam.amazonaws.com"),
                ("x-amz-date", "20150830T123600Z"),
            ],
            body: b"",
        };
        let auth = authorization(
            &r,
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "iam",
            ("20150830T123600Z", "20150830"),
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }
}
