//! Decision image acquisition uses the shared public media boundary.
use super::{OpenAiError, invalid};
use scala_engine::{DecisionContent, DecisionRequest};
use serde_json::Value;
use std::time::Duration;
pub(super) async fn acquire(request: &mut DecisionRequest) -> Result<(), OpenAiError> {
    let acquired = tokio::time::timeout(Duration::from_secs(30), async {
        let mut urls = Vec::new();
        for url in &request.images {
            // Keep the established Decision URL contract. Raw base64 is an
            // embedding extension, while Decision images require URL syntax.
            if !url.starts_with("data:")
                && !url.starts_with("http://")
                && !url.starts_with("https://")
            {
                return Err(invalid(
                    "Decision images require a data URI or public HTTP(S) URL.",
                    "input",
                ));
            }
            urls.push(crate::media::acquire(scala_engine::media::MediaKind::Image, url).await?);
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
