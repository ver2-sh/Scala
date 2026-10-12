//! Public image acquisition: bounded bodies, no proxy, redirects or private DNS.
use super::{OpenAiError, invalid};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use scala_engine::{DecisionContent, DecisionRequest, MAX_DECISION_IMAGE_BYTES};
use serde_json::Value;
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
fn raster(bytes: &[u8]) -> Result<&'static str, OpenAiError> {
    scala_engine::validate_decision_raster(bytes).map_err(|m| invalid(m, "input"))
}
async fn acquire_one(value: &str) -> Result<String, OpenAiError> {
    if value.starts_with("data:") {
        let (header, body) = value
            .split_once(',')
            .ok_or_else(|| invalid("Invalid image data URL.", "input"))?;
        if body.len() > MAX_DECISION_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(invalid("Image exceeds 8 MiB.", "input"));
        }
        let bytes = STANDARD
            .decode(body)
            .map_err(|_| invalid("Invalid image base64.", "input"))?;
        if bytes.len() > MAX_DECISION_IMAGE_BYTES {
            return Err(invalid("Image exceeds 8 MiB.", "input"));
        }
        if header != format!("data:{};base64", raster(&bytes)?) {
            return Err(invalid(
                "Image media type does not match its bytes.",
                "input",
            ));
        }
        return Ok(value.into());
    }
    let url = url::Url::parse(value)
        .map_err(|_| invalid("Image must be a data URL or public HTTP(S) URL.", "input"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err(invalid(
            "Image URL must use HTTP(S), a standard port and no credentials or fragment.",
            "input",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("Image URL is missing a host.", "input"))?;
    let port = url.port_or_known_default().unwrap();
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| invalid("Image host could not be resolved.", "input"))?
        .collect();
    if addresses.is_empty()
        || addresses.len() > 32
        || addresses.iter().any(|a| !public_address(a.ip()))
    {
        return Err(invalid(
            "Image host must resolve exclusively to public network addresses.",
            "input",
        ));
    }
    // Pin the validated lookup. Disabling proxies closes ambient proxy and DNS
    // rebinding bypasses; redirects require a separate validated request and are rejected.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| invalid("Image acquisition is unavailable.", "input"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| invalid("Public image acquisition failed or timed out.", "input"))?;
    if !response.status().is_success() {
        return Err(invalid(
            "Image URL must return a successful response directly; redirects are unsupported.",
            "input",
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_DECISION_IMAGE_BYTES as u64)
    {
        return Err(invalid("Image exceeds 8 MiB.", "input"));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().trim().to_owned());
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid("Image download failed.", "input"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_DECISION_IMAGE_BYTES {
            return Err(invalid("Image exceeds 8 MiB.", "input"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let actual = raster(&bytes)?;
    if mime.as_deref() != Some(actual) {
        return Err(invalid(
            "Image Content-Type must match its raster bytes.",
            "input",
        ));
    }
    Ok(format!("data:{actual};base64,{}", STANDARD.encode(bytes)))
}
pub(super) async fn acquire(request: &mut DecisionRequest) -> Result<(), OpenAiError> {
    let acquired = tokio::time::timeout(Duration::from_secs(30), async {
        let mut urls = Vec::new();
        for url in &request.images {
            urls.push(acquire_one(url).await?);
        }
        Ok::<_, OpenAiError>(urls)
    })
    .await
    .map_err(|_| invalid("Image acquisition exceeded the 30 second limit.", "input"))??;
    if let DecisionContent::Array(messages) = &mut request.state {
        let mut index = 0;
        for message in messages {
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in parts {
                    if let Some(url) = part.pointer_mut("/image_url/url") {
                        *url = Value::String(acquired[index].clone());
                        index += 1;
                    }
                }
            }
        }
    }
    request.images = acquired;
    Ok(())
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
            assert!(acquire_one(url).await.is_err());
        }
    }
}
