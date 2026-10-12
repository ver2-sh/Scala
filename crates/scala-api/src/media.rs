//! Public media acquisition: bounded bodies, no proxy, redirects or private DNS.
use crate::error::OpenAiError;
fn invalid(message: impl Into<String>, parameter: &str) -> OpenAiError {
    OpenAiError::invalid(message, Some(parameter), "invalid_value")
}
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use scala_engine::media::{MAX_MEDIA_BYTES, MediaKind, validate_bytes, validate_inline};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Admit only global unicast, exclude transition/documentation ranges.
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2002
                && s[0] != 0x3fff
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && ip.to_ipv4_mapped().is_none()
        }
    }
}
pub(crate) async fn acquire(kind: MediaKind, value: &str) -> Result<String, OpenAiError> {
    tokio::time::timeout(Duration::from_secs(15), acquire_inner(kind, value))
        .await
        .map_err(|_| invalid("Media acquisition exceeded 15 seconds.", "input"))?
}
async fn acquire_inner(kind: MediaKind, value: &str) -> Result<String, OpenAiError> {
    if value.starts_with("data:") || !value.contains(':') {
        validate_inline(kind, value).map_err(|m| invalid(m, "input"))?;
        return Ok(value.into());
    }
    let url = url::Url::parse(value)
        .map_err(|_| invalid("Media must be a data URL or public HTTP(S) URL.", "input"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err(invalid(
            "Media URL must use HTTP(S), a standard port and no credentials or fragment.",
            "input",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("Media URL is missing a host.", "input"))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    if matches!(host, "localhost" | "metadata.google.internal")
        || [".localhost", ".local", ".internal", ".ts.net"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
    {
        return Err(invalid(
            "Private/overlay media hosts are unsupported.",
            "input",
        ));
    }
    let port = url.port_or_known_default().unwrap();
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| invalid("Media host could not be resolved.", "input"))?
        .take(33)
        .collect();
    validate_addresses(&addresses)?;
    // Pin the validated lookup. Disabling proxies closes ambient proxy and DNS
    // rebinding bypasses; redirects require a separate validated request and are rejected.
    let client = pinned_client(host, &addresses)
        .build()
        .map_err(|_| invalid("Media acquisition is unavailable.", "input"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| invalid("Public media acquisition failed or timed out.", "input"))?;
    if !response.status().is_success() {
        return Err(invalid(
            "Media URL must return a successful response directly; redirects are unsupported.",
            "input",
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_MEDIA_BYTES as u64)
    {
        return Err(invalid("Media exceeds 8 MiB.", "input"));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().trim().to_owned());
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid("Media download failed.", "input"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_MEDIA_BYTES {
            return Err(invalid("Media exceeds 8 MiB.", "input"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let actual = validate_bytes(kind, &bytes).map_err(|m| invalid(m, "input"))?;
    if mime.as_deref() != Some(actual) {
        return Err(invalid(
            "Media Content-Type must match its validated bytes.",
            "input",
        ));
    }
    Ok(format!("data:{actual};base64,{}", STANDARD.encode(bytes)))
}

fn validate_addresses(addresses: &[SocketAddr]) -> Result<(), OpenAiError> {
    if addresses.is_empty()
        || addresses.len() > 32
        || addresses.iter().any(|a| !public_address(a.ip()))
    {
        return Err(invalid(
            "Media host must resolve exclusively to public network addresses.",
            "input",
        ));
    }
    Ok(())
}
fn pinned_client(host: &str, addresses: &[SocketAddr]) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, addresses)
        .timeout(Duration::from_secs(15))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_nonpublic_ranges_including_mapped_and_transition_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_address(ip.parse().unwrap()), "{ip}");
        }
    }
    #[tokio::test]
    async fn private_urls_and_malformed_rasters_fail_without_dispatch() {
        for url in [
            "http://127.0.0.1/x",
            "http://169.254.169.254/x",
            "file:///etc/passwd",
            "http://user:password@example.com/x",
            "https://example.com:8443/x",
            "data:image/png;base64,YQ==",
        ] {
            assert!(acquire(MediaKind::Image, url).await.is_err());
        }
    }
    #[tokio::test]
    async fn embedding_dns_rebinding_and_redirects_cannot_replace_pinned_addresses() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Rebinding(Arc<AtomicUsize>);
        impl reqwest::dns::Resolve for Rebinding {
            fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(Box::new(std::iter::once(
                        "169.254.169.254:80".parse::<SocketAddr>().unwrap(),
                    )) as reqwest::dns::Addrs)
                })
            }
        }
        let mixed = [
            "8.8.8.8:80".parse().unwrap(),
            "100.64.0.1:80".parse().unwrap(),
        ];
        assert!(validate_addresses(&mixed).is_err());
        assert!(validate_addresses(&mixed[..1]).is_ok());
        // Only test the post-validation connector using a loopback fixture.
        // Production validation above forbids this address. The hostile DNS
        // resolver must never execute once a validated result has been pinned.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(|| async {
                    axum::response::Redirect::temporary("http://169.254.169.254/credentials")
                }),
            )
            .await
            .unwrap();
        });
        let lookups = Arc::new(AtomicUsize::new(0));
        let client = pinned_client("rebind.test", &[address])
            .dns_resolver(Arc::new(Rebinding(lookups.clone())))
            .build()
            .unwrap();
        let response = client
            .get(format!("http://rebind.test:{}/media", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(lookups.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
