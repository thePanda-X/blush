//! The web app's files, built by `bun run build` in `web/` and embedded at
//! compile time (see build.rs).

pub struct Asset {
    pub bytes: &'static [u8],
    pub mime: &'static str,
    pub etag: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub fn get(path: &str) -> Option<Asset> {
    let path = if path.is_empty() { "index.html" } else { path };
    ASSETS.iter().find(|(p, ..)| *p == path).map(|&(_, bytes, mime, etag)| Asset { bytes, mime, etag })
}
