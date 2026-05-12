// Image attachments for input boxes that send to a claude child.
//
// Three input pathways feed `AttachmentSet`:
//   1. Ctrl+V — `try_from_clipboard()` (OS clipboard image bytes or image
//      file path).
//   2. Bracketed paste — `try_from_paste(content)` (Cmd+V, drag-drop,
//      large text). Strips `file://` prefix, splits on whitespace,
//      attaches every recognised image path. If nothing is recognised the
//      caller falls back to inserting `content` as text.
//   3. Drag-drop — falls through #2; macOS terminals deliver drops as
//      bracketed paste content.
//
// Outgoing IPC: on send, `AttachmentSet::take_all()` is combined with the
// user's text into a stream-json `user` message whose `content` is an
// array of `text` + `image` blocks (claude SDK content-block shape).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use base64::Engine;

const IMAGE_EXTS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

#[derive(Debug, Clone)]
pub struct Attachment {
    pub source_path: Option<PathBuf>,
    pub display_name: String,
    pub mime: String,
    pub data_b64: String,
}

impl Attachment {
    /// Serialize as a stream-json `image` content block.
    pub fn to_content_block(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": self.mime,
                "data": self.data_b64,
            }
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AttachmentSet(Vec<Attachment>);

impl AttachmentSet {
    pub fn new() -> Self {
        Self(Vec::new())
    }
    pub fn push(&mut self, a: Attachment) {
        self.0.push(a);
    }
    pub fn remove(&mut self, idx: usize) {
        if idx < self.0.len() {
            self.0.remove(idx);
        }
    }
    pub fn pop(&mut self) -> Option<Attachment> {
        self.0.pop()
    }
    pub fn take_all(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.0)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Attachment> {
        self.0.iter()
    }
}

/// Build the stream-json `user` message body. If `attachments` is empty,
/// emit the old text-only shape so we don't churn IPC for plain chats.
/// Otherwise emit a content-block array: text first, then images.
pub fn build_user_message(text: &str, attachments: &[Attachment]) -> serde_json::Value {
    if attachments.is_empty() {
        return serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": text }
        });
    }
    let mut blocks: Vec<serde_json::Value> = Vec::with_capacity(1 + attachments.len());
    // Always emit a text block — even an empty one keeps the shape uniform
    // for the receiver and matches claude SDK examples.
    blocks.push(serde_json::json!({ "type": "text", "text": text }));
    for a in attachments {
        blocks.push(a.to_content_block());
    }
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": blocks }
    })
}

/// Read an image file from disk + encode as an Attachment.
pub fn from_image_path(path: &Path) -> Result<Attachment> {
    let mime = mime_for_path(path)
        .ok_or_else(|| anyhow!("not a supported image: {}", path.display()))?;
    let bytes = std::fs::read(path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let display_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string());
    Ok(Attachment {
        source_path: Some(path.to_path_buf()),
        display_name,
        mime: mime.to_string(),
        data_b64,
    })
}

fn mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_EXTS.iter().find_map(|(e, m)| if *e == ext { Some(*m) } else { None })
}

/// Strip `file://` URI prefix, percent-decode, drop surrounding quotes.
fn normalise_token(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // unquote
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_string();
        // very basic %-decoding for spaces — terminals send `%20` for
        // spaces in dropped paths.
        s = s.replace("%20", " ");
    }
    s
}

/// Parse a bracketed paste payload. Returns one Result per recognised
/// image path. An empty Vec means the payload had no image paths — the
/// caller treats it as plain text.
pub fn try_from_paste(content: &str) -> Vec<Result<Attachment>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Heuristic: split on newlines + whitespace. A real image path won't
    // contain whitespace once unquoted (drag-drop with spaces arrives as a
    // single quoted token, handled by normalise_token).
    let mut tokens: Vec<String> = Vec::new();
    // First try newline-separated paths (drag-drop of multiple files).
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // If a single line is a quoted/file:// token (drag-drop of a path
        // containing spaces), take it whole. Quotes are a strong signal —
        // a bare whitespace-containing line is treated as separate tokens.
        let trimmed_line = line.trim();
        let quoted = (trimmed_line.starts_with('"') && trimmed_line.ends_with('"'))
            || (trimmed_line.starts_with('\'') && trimmed_line.ends_with('\''));
        if quoted {
            let n = normalise_token(trimmed_line);
            if Path::new(&n).is_absolute() && mime_for_path(Path::new(&n)).is_some() {
                tokens.push(n);
                continue;
            }
        }
        // Single token line that's a recognisable path → take it whole.
        if !trimmed_line.contains(char::is_whitespace) {
            let n = normalise_token(trimmed_line);
            if Path::new(&n).is_absolute() && mime_for_path(Path::new(&n)).is_some() {
                tokens.push(n);
                continue;
            }
        }
        // Otherwise split on whitespace (drop of several files on one line).
        for part in line.split_whitespace() {
            tokens.push(normalise_token(part));
        }
    }

    let mut out = Vec::new();
    for t in &tokens {
        let p = Path::new(t);
        if p.is_absolute() && mime_for_path(p).is_some() {
            out.push(from_image_path(p));
        }
    }
    out
}

/// Ctrl+V entry: ask the OS clipboard for either image bytes or text. If
/// neither is an image we return None so the caller can insert text or
/// fall through. Returns Some(Err) when something looked like an image
/// but failed to encode — surfaces the error to the user.
pub fn try_from_clipboard() -> Option<Result<Attachment>> {
    let mut cb = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => return Some(Err(anyhow!("clipboard unavailable: {e}"))),
    };

    if let Ok(img) = cb.get_image() {
        return Some(encode_clipboard_image(img));
    }

    if let Ok(text) = cb.get_text() {
        let trimmed = text.trim();
        let normalised = normalise_token(trimmed);
        let p = Path::new(&normalised);
        if p.is_absolute() && mime_for_path(p).is_some() {
            return Some(from_image_path(p));
        }
    }
    None
}

fn encode_clipboard_image(img: arboard::ImageData<'_>) -> Result<Attachment> {
    // arboard yields raw RGBA8 + dims; we re-encode as PNG so the receiver
    // gets a normal file format. PNG keeps it loss-less and small enough
    // for typical screenshots.
    let png_bytes = rgba_to_png(&img.bytes, img.width as u32, img.height as u32)?;
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok(Attachment {
        source_path: None,
        display_name: format!("clipboard-{}x{}.png", img.width, img.height),
        mime: "image/png".to_string(),
        data_b64,
    })
}

/// Minimal raw-RGBA → PNG encoder so we don't pull in the `image` crate.
/// PNG = 8-bit RGBA, no interlace, one IDAT, zlib-store-only deflate (no
/// compression). Screenshots are typically small enough that "no
/// compression" stays well under 50 MB.
fn rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return Err(anyhow!(
            "rgba buffer size {} doesn't match {}x{}",
            rgba.len(),
            width,
            height
        ));
    }
    let mut out: Vec<u8> = Vec::with_capacity(rgba.len() + 1024);
    // PNG signature.
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR.
    let mut ihdr: Vec<u8> = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Raw image data with per-row filter byte 0.
    let row_bytes = (width as usize) * 4;
    let mut raw: Vec<u8> = Vec::with_capacity((row_bytes + 1) * (height as usize));
    for y in 0..(height as usize) {
        raw.push(0);
        let start = y * row_bytes;
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }

    // zlib container around uncompressed deflate blocks.
    let zlib = zlib_store(&raw);
    write_chunk(&mut out, b"IDAT", &zlib);

    // IEND.
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    // Standard CRC-32 (poly 0xEDB88320). Inlined to avoid a crate dep.
    let mut table = [0u32; 256];
    for n in 0..256u32 {
        let mut c = n;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        table[n as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    // zlib header for "deflate, no compression, default" + stored blocks.
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 16);
    out.push(0x78);
    out.push(0x01);
    let max_block = 65_535usize;
    let mut i = 0;
    while i < data.len() {
        let remaining = data.len() - i;
        let block_len = remaining.min(max_block);
        let final_block = remaining <= max_block;
        out.push(if final_block { 0x01 } else { 0x00 });
        out.push((block_len & 0xFF) as u8);
        out.push(((block_len >> 8) & 0xFF) as u8);
        let nlen = !(block_len as u16);
        out.push((nlen & 0xFF) as u8);
        out.push(((nlen >> 8) & 0xFF) as u8);
        out.extend_from_slice(&data[i..i + block_len]);
        i += block_len;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Minimal 1x1 red PNG, hex-decoded. Just enough bytes to be a real PNG
    // file so `from_image_path` succeeds.
    const PNG_1X1_RED: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0x99, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x5B, 0xCC, 0x6C, 0x09, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn write_png(name: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{name}"))
            .tempfile()
            .unwrap();
        f.write_all(PNG_1X1_RED).unwrap();
        f
    }

    #[test]
    fn from_image_path_encodes_png() {
        let f = write_png("png");
        let a = from_image_path(f.path()).unwrap();
        assert_eq!(a.mime, "image/png");
        // Base64 of a 70-byte file is ~96 chars (4/3 ratio).
        assert!(a.data_b64.len() > 80);
        assert_eq!(a.source_path.as_deref(), Some(f.path()));
        // Decoding the base64 must round-trip to the original bytes.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&a.data_b64)
            .unwrap();
        assert_eq!(decoded, PNG_1X1_RED);
    }

    #[test]
    fn from_image_path_rejects_non_image() {
        let f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        assert!(from_image_path(f.path()).is_err());
    }

    #[test]
    fn try_from_paste_single_path() {
        let f = write_png("png");
        let out = try_from_paste(f.path().to_str().unwrap());
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
    }

    #[test]
    fn try_from_paste_file_uri_prefix() {
        let f = write_png("png");
        let payload = format!("file://{}", f.path().display());
        let out = try_from_paste(&payload);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
    }

    #[test]
    fn try_from_paste_quoted_path() {
        let f = write_png("png");
        let payload = format!("'{}'", f.path().display());
        let out = try_from_paste(&payload);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
    }

    #[test]
    fn try_from_paste_multiple_paths_whitespace() {
        let f1 = write_png("png");
        let f2 = write_png("png");
        let payload = format!(
            "{} {}",
            f1.path().display(),
            f2.path().display()
        );
        let out = try_from_paste(&payload);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.is_ok()));
    }

    #[test]
    fn try_from_paste_multiple_paths_newlines() {
        let f1 = write_png("png");
        let f2 = write_png("jpg"); // .jpg suffix path that isn't a real jpeg — still parsed as mime
        // ensure jpg extension is detected even though file content is PNG.
        let payload = format!("{}\n{}", f1.path().display(), f2.path().display());
        let out = try_from_paste(&payload);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn try_from_paste_plain_text_yields_no_attachments() {
        let out = try_from_paste("hello world this is just text");
        assert!(out.is_empty());
    }

    #[test]
    fn try_from_paste_mixed_text_and_path_attaches_path_only() {
        let f = write_png("png");
        let payload = format!("look at this {}", f.path().display());
        let out = try_from_paste(&payload);
        // Path gets extracted; other tokens are dropped (caller decides
        // whether to also insert original text).
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
    }

    #[test]
    fn try_from_paste_non_image_path_drops() {
        // Non-image extension: not an attachment.
        let f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        let out = try_from_paste(f.path().to_str().unwrap());
        assert!(out.is_empty());
    }

    #[test]
    fn attachment_set_basic() {
        let mut s = AttachmentSet::new();
        assert!(s.is_empty());
        let f = write_png("png");
        s.push(from_image_path(f.path()).unwrap());
        s.push(from_image_path(f.path()).unwrap());
        assert_eq!(s.len(), 2);
        s.pop();
        assert_eq!(s.len(), 1);
        let taken = s.take_all();
        assert_eq!(taken.len(), 1);
        assert!(s.is_empty());
    }

    #[test]
    fn build_user_message_text_only_keeps_string_content() {
        let v = build_user_message("hi", &[]);
        assert_eq!(v.pointer("/message/content").and_then(|x| x.as_str()), Some("hi"));
    }

    #[test]
    fn build_user_message_with_image_emits_blocks() {
        let f = write_png("png");
        let a = from_image_path(f.path()).unwrap();
        let v = build_user_message("look", &[a]);
        let arr = v
            .pointer("/message/content")
            .and_then(|x| x.as_array())
            .unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "look");
        assert_eq!(arr[1]["type"], "image");
        assert_eq!(arr[1]["source"]["media_type"], "image/png");
        assert!(arr[1]["source"]["data"].as_str().unwrap().len() > 80);
    }

    #[test]
    fn rgba_to_png_round_trips_header() {
        // Build a 2x2 RGBA buffer, encode, and check the signature + IHDR.
        let rgba = vec![0u8; 2 * 2 * 4];
        let png = rgba_to_png(&rgba, 2, 2).unwrap();
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // IHDR width=2, height=2 in big-endian after 8-byte signature +
        // 4-byte length + 4-byte type.
        assert_eq!(&png[16..20], &[0, 0, 0, 2]);
        assert_eq!(&png[20..24], &[0, 0, 0, 2]);
    }
}
