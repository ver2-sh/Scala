//! Shared bounded transport validation. Never alter bytes or compute embeddings.
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub const MAX_MEDIA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

pub fn validate_inline(kind: MediaKind, value: &str) -> Result<(&'static str, Vec<u8>), String> {
    let (declared, payload) = if value.starts_with("data:") {
        let (header, payload) = value.split_once(',').ok_or("Invalid media data URI.")?;
        let mime = header
            .strip_prefix("data:")
            .and_then(|s| s.strip_suffix(";base64"))
            .ok_or("Media data URI must use a MIME type and base64 encoding.")?;
        (Some(mime), payload)
    } else {
        // handle_media accepts raw base64 for all three native part types.
        (None, value)
    };
    if payload.len() > MAX_MEDIA_BYTES.div_ceil(3) * 4 {
        return Err("Media exceeds 8 MiB.".into());
    }
    let bytes = STANDARD
        .decode(payload)
        .map_err(|_| "Invalid media base64; local paths and unacquired URLs are unsupported.")?;
    let mime = validate_bytes(kind, &bytes)?;
    if declared.is_some_and(|m| m != mime) {
        return Err("Media type does not match the validated bytes.".into());
    }
    Ok((mime, bytes))
}

pub fn validate_bytes(kind: MediaKind, bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.is_empty() || bytes.len() > MAX_MEDIA_BYTES {
        return Err("Media must contain 1 byte through 8 MiB.".into());
    }
    match kind {
        MediaKind::Image => validate_raster(bytes),
        MediaKind::Audio => {
            validate_wav(bytes)?;
            Ok("audio/wav")
        }
        MediaKind::Video => {
            validate_avi(bytes)?;
            Ok("video/x-msvideo")
        }
    }
}

pub fn validate_raster(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.len() > MAX_MEDIA_BYTES {
        return Err("image exceeds 8 MiB".into());
    }
    let format = image::guess_format(bytes).map_err(|_| "invalid image format")?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("only PNG, JPEG and WebP are admitted".into()),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| "malformed image or image exceeds 4096 by 4096 / 64 MiB decode limit")?;
    Ok(mime)
}

fn le16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("Truncated media header.")?
            .try_into()
            .unwrap(),
    ))
}
fn le32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("Truncated media header.")?
            .try_into()
            .unwrap(),
    ))
}
fn riff<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> Result<&'a [u8], String> {
    if bytes.get(..4) != Some(b"RIFF")
        || bytes.get(8..12) != Some(kind)
        || le32(bytes, 4)? as usize + 8 != bytes.len()
    {
        return Err("Only complete PCM WAV audio or self-contained uncompressed AVI video is admitted; other containers cannot be bounded safely by this native decoder.".into());
    }
    Ok(&bytes[12..])
}
// No recursive or opaque containers, external references, compressed frames,
// playlists or codec extradata reach native FFmpeg. Bound header work as well.
type RiffChunk<'a> = (&'a [u8], &'a [u8]);
fn chunks(mut bytes: &[u8]) -> Result<Vec<RiffChunk<'_>>, String> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        if result.len() == 4096 {
            return Err("Too many media chunks.".into());
        }
        let id = bytes.get(..4).ok_or("Truncated media chunk.")?;
        let len = le32(bytes, 4)? as usize;
        let payload = bytes.get(8..8 + len).ok_or("Truncated media chunk.")?;
        result.push((id, payload));
        bytes = bytes
            .get(8 + len + (len % 2)..)
            .ok_or("Truncated media padding.")?;
    }
    Ok(result)
}

fn validate_wav(bytes: &[u8]) -> Result<(), String> {
    let mut fmt = None;
    let mut data = None;
    for (id, part) in chunks(riff(bytes, b"WAVE")?)? {
        match id {
            b"fmt " if fmt.is_none() => fmt = Some(part),
            b"data" if data.is_none() && fmt.is_some() => data = Some(part),
            b"JUNK" => {},
            _ => return Err("WAV accepts one fmt and one data chunk; compressed codecs and opaque metadata are unsupported.".into()),
        }
    }
    let fmt = fmt.ok_or("WAV is missing fmt.")?;
    let data = data.ok_or("WAV is missing data.")?;
    if !(fmt.len() == 16 || (fmt.len() == 18 && le16(fmt, 16)? == 0)) || le16(fmt, 0)? != 1 {
        return Err("WAV requires integer PCM (8, 16, 24 or 32 bits); compressed and floating codecs are unsupported.".into());
    }
    let channels = le16(fmt, 2)? as usize;
    let rate = le32(fmt, 4)? as usize;
    let bits = le16(fmt, 14)? as usize;
    if !(1..=2).contains(&channels)
        || !(8000..=192000).contains(&rate)
        || !matches!(bits, 8 | 16 | 24 | 32)
    {
        return Err("WAV requires 1–2 channels, 8–192 kHz and 8/16/24/32-bit integer PCM.".into());
    }
    let align = channels * (bits / 8);
    if le16(fmt, 12)? as usize != align
        || le32(fmt, 8)? as usize != rate * align
        || data.is_empty()
        || data.len() % align != 0
        || data.len() / align > rate * 60
    {
        return Err("WAV PCM length/alignment is invalid or duration exceeds 60 seconds.".into());
    }
    Ok(())
}

fn validate_avi(bytes: &[u8]) -> Result<(), String> {
    let mut headers = None;
    let mut frames = None;
    for (id, part) in chunks(riff(bytes, b"AVI ")?)? {
        match (id, part.get(..4)) {
            (b"LIST", Some(b"hdrl")) if headers.is_none() && frames.is_none() => headers = Some(&part[4..]),
            (b"LIST", Some(b"movi")) if frames.is_none() && headers.is_some() => frames = Some(&part[4..]),
            (b"JUNK", _) => {},
            // Indexes contain file offsets. Reject them rather than allowing
            // native seek behavior to contradict the fully inspected frame list.
            _ => return Err("AVI requires a single hdrl and movi list; indexes, extra streams and opaque containers are unsupported.".into()),
        }
    }
    let mut avih = None;
    let mut strl = None;
    for (id, part) in chunks(headers.ok_or("AVI is missing hdrl.")?)? {
        match (id, part.get(..4)) {
            (b"avih", _) if avih.is_none() => avih = Some(part),
            (b"LIST", Some(b"strl")) if strl.is_none() => strl = Some(&part[4..]),
            (b"JUNK", _) => {}
            _ => return Err("AVI requires one video stream and bounded headers.".into()),
        }
    }
    let avih = avih.ok_or("AVI is missing avih.")?;
    let mut strh = None;
    let mut strf = None;
    for (id, part) in chunks(strl.ok_or("AVI is missing strl.")?)? {
        match id {
            b"strh" if strh.is_none() => strh = Some(part),
            b"strf" if strf.is_none() => strf = Some(part),
            b"JUNK" => {}
            _ => return Err("AVI codec extensions and secondary streams are unsupported.".into()),
        }
    }
    let strh = strh.ok_or("AVI is missing strh.")?;
    let strf = strf.ok_or("AVI is missing strf.")?;
    let width = le32(strf, 4)? as usize;
    let height = (le32(strf, 8)? as i32).unsigned_abs() as usize;
    let scale = le32(strh, 20)? as u64;
    let rate = le32(strh, 24)? as u64;
    let count = le32(strh, 32)? as usize;
    if avih.len() != 56
        || strh.len() != 56
        || strf.len() != 40
        || le32(strf, 0)? != 40
        || strh.get(..4) != Some(b"vids")
        || !matches!(strh.get(4..8), Some(b"DIB " | b"\0\0\0\0"))
        || le32(strf, 16)? != 0
        || le16(strf, 12)? != 1
        || le16(strf, 14)? != 24
        || !(1..=1024).contains(&width)
        || !(1..=1024).contains(&height)
        || !(1..=64).contains(&count)
        || scale == 0
        || rate == 0
        || rate < scale
        || rate > scale * 60
        || count as u64 * scale > rate * 10
        || le32(strh, 28)? != 0
        || le32(avih, 16)? as usize != count
        || le32(avih, 24)? != 1
        || le32(avih, 32)? as usize != width
        || le32(avih, 36)? as usize != height
        || le32(avih, 12)? != 0
        || le32(avih, 0)? as u64 != 1_000_000 * scale / rate
        || le32(strf, 32)? != 0
        || le32(strf, 36)? != 0
    {
        return Err("AVI requires one raw 24-bit BGR stream, 1–1024 pixel dimensions, 1–64 frames, 1–60 fps and at most 10 seconds, with consistent headers.".into());
    }
    let frame_bytes = (width * 3).div_ceil(4) * 4 * height;
    if le32(strf, 20)? as usize != frame_bytes {
        return Err("AVI bitmap size is inconsistent.".into());
    }
    let frames = chunks(frames.ok_or("AVI is missing movi.")?)?;
    if frames.len() != count
        || frames
            .iter()
            .any(|(id, frame)| *id != b"00db" || frame.len() != frame_bytes)
    {
        return Err("AVI frame count or uncompressed frame size is inconsistent.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const WAV: &[u8] = include_bytes!("../testdata/embedding.wav");
    const AVI: &[u8] = include_bytes!("../testdata/embedding.avi");
    #[test]
    fn embedding_media_preserves_valid_bytes_and_rejects_wrong_types() {
        for (kind, mime, bytes) in [
            (MediaKind::Audio, "audio/wav", WAV),
            (MediaKind::Video, "video/x-msvideo", AVI),
        ] {
            assert_eq!(validate_bytes(kind, bytes).unwrap(), mime);
            for value in [
                STANDARD.encode(bytes),
                format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            ] {
                assert_eq!(
                    validate_inline(kind, &value).unwrap(),
                    (mime, bytes.to_vec())
                );
            }
            for other in [MediaKind::Image, MediaKind::Audio, MediaKind::Video] {
                if other != kind {
                    assert!(validate_bytes(other, bytes).is_err());
                }
            }
            for n in 0..bytes.len() {
                assert!(validate_bytes(kind, &bytes[..n]).is_err());
            }
        }
    }
    #[test]
    fn embedding_media_rejects_invalid_base64_mime_and_size() {
        for value in [
            "",
            "!!!!",
            "YQ",
            "file:///etc/passwd",
            "http://127.0.0.1/a",
            "data:audio/wav;utf8,AAAA",
            "data:audio/mp3;base64,AAAA",
        ] {
            assert!(validate_inline(MediaKind::Audio, value).is_err());
        }
        let oversized = STANDARD.encode(vec![0; MAX_MEDIA_BYTES + 1]);
        assert!(
            validate_inline(MediaKind::Audio, &oversized)
                .unwrap_err()
                .contains("8 MiB")
        );
        let wrong = format!("data:audio/mp3;base64,{}", STANDARD.encode(WAV));
        assert!(validate_inline(MediaKind::Audio, &wrong).is_err());
        for bytes in [
            b"#EXTM3U\nhttp://169.254.169.254/x".as_slice(),
            b"<MPD><BaseURL>file:///etc/passwd</BaseURL></MPD>",
        ] {
            assert!(validate_bytes(MediaKind::Video, bytes).is_err());
        }
    }
    #[test]
    fn embedding_decoded_bounds_and_container_headers_are_authoritative() {
        let mut wav = WAV.to_vec();
        wav[34..36].copy_from_slice(&64u16.to_le_bytes());
        assert!(validate_bytes(MediaKind::Audio, &wav).is_err());
        let mut long = WAV[..44].to_vec();
        long.resize(44 + 8000 * 2 * 61, 0);
        let size = long.len();
        long[4..8].copy_from_slice(&((size - 8) as u32).to_le_bytes());
        long[40..44].copy_from_slice(&((size - 44) as u32).to_le_bytes());
        assert!(
            validate_bytes(MediaKind::Audio, &long)
                .unwrap_err()
                .contains("60 seconds")
        );
        // Each important native decoder field must reject a conflicting value.
        for marker in [b"avih", b"strh", b"strf", b"00db"] {
            let mut video = AVI.to_vec();
            let at = video.windows(4).position(|v| v == marker).unwrap();
            video[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(validate_bytes(MediaKind::Video, &video).is_err());
        }
        for (relative, n) in [
            (4, 4096u32),
            (8, u32::MAX),
            (16, 0x34363248),
            (20, u32::MAX),
        ] {
            let mut video = AVI.to_vec();
            let at = video.windows(4).position(|v| v == b"strf").unwrap() + 8 + relative;
            video[at..at + 4].copy_from_slice(&n.to_le_bytes());
            assert!(validate_bytes(MediaKind::Video, &video).is_err());
        }
    }
}
