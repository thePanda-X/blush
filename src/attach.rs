//! Prompt attachments: images (pasted from the clipboard, or an image file
//! path dropped into the prompt) and long pastes collapsed into a
//! placeholder. The prompt shows a short `[Image #1]` / `[Pasted text #2 …]`
//! token; it's expanded when the prompt is sent.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Pastes at least this many lines, or characters, collapse into a placeholder.
const COLLAPSE_LINES: usize = 12;
const COLLAPSE_CHARS: usize = 1500;
/// Claude rejects images over 5 MB once base64-encoded.
const MAX_IMAGE_BYTES: usize = 3_700_000;

#[derive(Clone, Debug)]
pub enum Attachment {
    /// An ACP image content block.
    Image(Value),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Attached {
    pub label: String,
    pub item: Attachment,
}

/// Attachments for the prompt being edited.
#[derive(Default)]
pub struct Attachments {
    pub items: Vec<Attached>,
    next: usize,
}

impl Attachments {
    pub fn add_image(&mut self, block: Value) -> String {
        self.next += 1;
        let label = format!("[Image #{}]", self.next);
        self.items.push(Attached { label: label.clone(), item: Attachment::Image(block) });
        label
    }

    pub fn add_text(&mut self, text: String) -> String {
        self.next += 1;
        let lines = text.lines().count();
        let label = format!("[Pasted text #{} +{lines} lines]", self.next);
        self.items.push(Attached { label: label.clone(), item: Attachment::Text(text) });
        label
    }

    /// Put back an image from a queued prompt that's being edited again.
    pub fn restore(&mut self, a: Attached) {
        if let Some(n) = a.label.trim_end_matches(']').rsplit('#').next().and_then(|n| n.split(' ').next()?.parse::<usize>().ok()) {
            self.next = self.next.max(n);
        }
        self.items.retain(|x| x.label != a.label);
        self.items.push(a);
    }

    /// Expand pasted-text placeholders in `text` and collect the images it
    /// still mentions. Everything is consumed: the next prompt starts fresh.
    pub fn take(&mut self, text: &str) -> (String, Vec<Attached>) {
        let mut out = text.to_string();
        let mut images = Vec::new();
        for a in self.items.drain(..) {
            if !out.contains(&a.label) {
                continue;
            }
            match &a.item {
                Attachment::Text(t) => out = out.replacen(&a.label, t, 1),
                Attachment::Image(_) => images.push(a),
            }
        }
        self.next = 0;
        (out, images)
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.next = 0;
    }
}

pub fn should_collapse(text: &str) -> bool {
    text.lines().count() >= COLLAPSE_LINES || text.chars().count() >= COLLAPSE_CHARS
}

fn mime_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// A pasted string that is just the path of an image file (drag and drop
/// pastes the path, sometimes quoted or `file://`-prefixed).
pub fn pasted_image_path(pasted: &str, cwd: &Path) -> Option<PathBuf> {
    let t = pasted.trim();
    if t.is_empty() || t.contains('\n') {
        return None;
    }
    let t = t.trim_matches(|c| c == '"' || c == '\'');
    let t = t.strip_prefix("file://").unwrap_or(t);
    // Shells escape spaces in dragged paths.
    let t = if cfg!(windows) { t.to_string() } else { t.replace("\\ ", " ") };
    let p = PathBuf::from(&t);
    let p = if p.is_absolute() { p } else { cwd.join(p) };
    (mime_for(&p).is_some() && p.is_file()).then_some(p)
}

pub fn image_block(mime: &str, bytes: &[u8]) -> Value {
    json!({"type": "image", "mimeType": mime, "data": base64(bytes)})
}

/// Read an image file into an ACP image block.
pub fn image_from_file(path: &Path) -> Result<Value, String> {
    let mime = mime_for(path).ok_or("not a PNG, JPEG, GIF or WebP image")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("image is {} MB; the limit is about 3.7 MB", bytes.len() / 1_000_000));
    }
    Ok(image_block(mime, &bytes))
}

/// The clipboard's image as a PNG image block.
pub fn image_from_clipboard() -> Result<Value, String> {
    let img = arboard::Clipboard::new().and_then(|mut c| c.get_image()).map_err(|e| match e {
        arboard::Error::ContentNotAvailable => "no image on the clipboard".to_string(),
        e => e.to_string(),
    })?;
    let png = encode_png(img.width as u32, img.height as u32, &img.bytes)?;
    if png.len() > MAX_IMAGE_BYTES {
        return Err(format!("clipboard image is {} MB as PNG; the limit is about 3.7 MB", png.len() / 1_000_000));
    }
    Ok(image_block("image/png", &png))
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_expand_and_images_follow() {
        let mut a = Attachments::default();
        let big = (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        assert!(should_collapse(&big) && !should_collapse("one line"));
        let t = a.add_text(big.clone());
        assert_eq!(t, "[Pasted text #1 +20 lines]");
        let i = a.add_image(json!({"type": "image"}));
        assert_eq!(i, "[Image #2]");
        let unused = a.add_image(json!({"type": "image"}));
        let (text, imgs) = a.take(&format!("look {t} and {i}"));
        assert_eq!(text, format!("look {big} and [Image #2]"));
        assert_eq!(imgs.len(), 1);
        assert!(!text.contains(&unused));
        assert!(a.items.is_empty());
        // Numbering restarts, but a restored image keeps its number reserved.
        a.restore(imgs[0].clone());
        assert_eq!(a.add_image(json!({})), "[Image #3]");
    }

    #[test]
    fn image_paths_and_encoding() {
        let dir = std::env::temp_dir().join("blush-attach-test");
        let _ = std::fs::create_dir_all(&dir);
        let png = dir.join("shot one.png");
        std::fs::write(&png, encode_png(1, 1, &[255, 0, 0, 255]).unwrap()).unwrap();
        let quoted = format!("\"{}\"", png.display());
        assert_eq!(pasted_image_path(&quoted, &dir), Some(png.clone()));
        assert_eq!(pasted_image_path("shot one.png", &dir), Some(png.clone()));
        assert_eq!(pasted_image_path("notes.txt", &dir), None);
        assert_eq!(pasted_image_path("two\nlines.png", &dir), None);
        let b = image_from_file(&png).unwrap();
        assert_eq!(b["mimeType"], "image/png");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
    }
}
