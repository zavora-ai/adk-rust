//! The files a chat message carried, downloaded bounded in size and type (spec 3.5 §10 X12,
//! §5 S18): images only, at most 10 MiB each. What is downloaded is
//! untrusted input; the kernel's gateway and its host decide whether it goes anywhere.

use std::time::Duration;

use futures::StreamExt;

/// The largest file taken, in bytes.
pub(crate) const MAX_BYTES: usize = 10 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether a file of `media_type` is taken: the image types the site's media pipeline reads.
pub(crate) fn allowed(media_type: &str) -> bool {
    matches!(media_type, "image/jpeg" | "image/png" | "image/webp" | "image/gif")
}

/// The media type in `value`, without its parameters, in lower case.
fn bare(value: &str) -> String {
    value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}

/// Downloads what `request` answers when it is an allowed image of at most [`MAX_BYTES`],
/// its type `declared` by the platform or else by the answer; `None` otherwise.
pub(crate) async fn download(
    request: reqwest::RequestBuilder,
    declared: Option<&str>,
) -> Option<(String, Vec<u8>)> {
    let response = match request.timeout(DOWNLOAD_TIMEOUT).send().await {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            tracing::debug!(
                http.status = response.status().as_u16(),
                "a chat file could not be downloaded"
            );
            return None;
        }
        Err(error) => {
            tracing::debug!(error = %error.without_url(), "a chat file could not be downloaded");
            return None;
        }
    };
    if response.content_length().is_some_and(|length| length > MAX_BYTES as u64) {
        tracing::debug!("a chat file larger than the limit was not downloaded");
        return None;
    }
    let answered = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(bare);
    let media_type = declared.map(bare).or(answered)?;
    if !allowed(&media_type) {
        tracing::debug!(media.type = %media_type, "a chat file of a type not taken was not downloaded");
        return None;
    }
    let mut bytes = Vec::new();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if bytes.len() + chunk.len() > MAX_BYTES {
            tracing::debug!("a chat file larger than the limit was dropped");
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some((media_type, bytes))
}

/// The most files taken from one message.
#[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
pub(crate) const MAX_FILES: usize = 10;

/// A file a message names, downloaded only when its account is paired (spec 3.5 §10 X12).
#[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRef {
    /// Where the platform serves it: an address, or Matrix's `mxc://` URI.
    pub(crate) url: String,
    /// The type the platform declares, if any.
    pub(crate) media_type: Option<String>,
    /// Its name.
    pub(crate) name: String,
    /// The size the platform declares, if any.
    pub(crate) size: Option<u64>,
}

#[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
impl FileRef {
    /// Whether the platform's own description already rules it out: a type not taken, or a
    /// size over the limit.
    pub(crate) fn ruled_out(&self) -> bool {
        self.media_type.as_deref().is_some_and(|media_type| !allowed(&bare(media_type)))
            || self.size.is_some_and(|size| size > MAX_BYTES as u64)
    }
}

/// Downloads at most [`MAX_FILES`] of `files` with `fetch`, as attachments; a file its
/// description rules out, of a type not taken or over the limit is left out.
#[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
pub(crate) async fn fetch_all<F, Fut>(
    files: &[FileRef],
    mut fetch: F,
) -> Vec<adk_kernel::channels::ChannelAttachment>
where
    F: FnMut(FileRef) -> Fut,
    Fut: Future<Output = Option<(String, Vec<u8>)>>,
{
    let mut attachments = Vec::new();
    for file in files.iter().take(MAX_FILES) {
        if file.ruled_out() {
            tracing::debug!("a chat file its platform describes as not taken was not downloaded");
            continue;
        }
        if let Some((media_type, bytes)) = fetch(file.clone()).await {
            let stem = file.name.rsplit_once('.').map_or(file.name.as_str(), |(stem, _)| stem);
            let name = name(stem, &media_type);
            attachments.push(adk_kernel::channels::ChannelAttachment::new(media_type, name, bytes));
        }
    }
    attachments
}

/// A file name for a download of `media_type` numbered `n`.
pub(crate) fn name(stem: &str, media_type: &str) -> String {
    let extension = media_type.rsplit('/').next().unwrap_or("bin").replace("jpeg", "jpg");
    let stem: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(48)
        .collect();
    format!("{}.{extension}", if stem.is_empty() { "photo" } else { &stem })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_images_are_taken_and_names_are_safe() {
        for media_type in ["image/jpeg", "image/png", "image/webp", "image/gif"] {
            assert!(allowed(media_type), "{media_type}");
        }
        for media_type in ["image/svg+xml", "application/pdf", "text/html", "video/mp4"] {
            assert!(!allowed(media_type), "{media_type}");
        }
        assert_eq!(bare("Image/PNG; charset=binary"), "image/png");
        assert_eq!(name("wamid.HBgM../../etc", "image/jpeg"), "wamidHBgMetc.jpg");
        assert_eq!(name("", "image/png"), "photo.png");
    }

    #[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
    #[tokio::test]
    async fn a_file_its_platform_rules_out_is_never_fetched() {
        let file = |media_type: Option<&str>, size: Option<u64>| FileRef {
            url: "https://files.example/a".into(),
            media_type: media_type.map(str::to_string),
            name: "rye.photo.png".into(),
            size,
        };
        let files = [
            file(Some("image/png"), Some(70)),
            file(Some("image/svg+xml"), Some(70)),
            file(Some("image/png"), Some(64 * 1024 * 1024)),
            file(None, None),
        ];
        let mut asked = 0;
        let taken = fetch_all(&files, |_| {
            asked += 1;
            async { Some(("image/png".to_string(), vec![1, 2, 3])) }
        })
        .await;
        assert_eq!(asked, 2, "the SVG and the oversized file were never fetched");
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].name, "ryephoto.png");
    }
}
