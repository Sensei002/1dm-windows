//! Website grabber: crawl a page (same-origin, bounded) and collect media /
//! document links matching user filters.

use std::collections::HashSet;

use dom_query::Document;
use serde::Serialize;
use url::Url;

#[derive(Debug, Clone, Serialize)]
pub struct GrabbedLink {
    pub url: String,
    pub text: String,
    pub ext: String,
}

/// Extracts media/document links from one HTML page.
pub fn extract_links(page_url: &str, html: &str) -> Vec<GrabbedLink> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let Ok(base) = Url::parse(page_url) else {
        return out;
    };

    let doc = Document::from(html);
    for el in doc.select("a[href]").iter() {
        let Some(raw) = el.attr("href") else { continue };
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with("javascript:") || raw.starts_with("mailto:") {
            continue;
        }
        let Ok(url) = base.join(raw) else { continue };
        let s = url.to_string();
        let ext = extension_of(&s);
        if ext.is_empty() || !is_media_ext(&ext) {
            continue;
        }
        if seen.insert(s.clone()) {
            let text = el.text().trim().to_string();
            out.push(GrabbedLink {
                url: s,
                text: if text.is_empty() { url.path().to_string() } else { text },
                ext,
            });
        }
    }
    out
}

/// Follows same-origin `next` pagination (`next_selector` matching the "next"
/// anchor) up to `max_pages`, aggregating links from every visited page.
pub async fn grab_site(
    client: &reqwest::Client,
    start_url: &str,
    next_selector: &str,
    max_pages: usize,
) -> Result<Vec<GrabbedLink>, reqwest::Error> {
    let mut links = Vec::new();
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut url = start_url.to_string();

    for _ in 0..max_pages.clamp(1, 50) {
        if !seen_pages.insert(url.clone()) {
            break;
        }
        let html = client.get(&url).send().await?.error_for_status()?.text().await?;
        links.extend(extract_links(&url, &html));

        if next_selector.is_empty() {
            break;
        }
        let doc = Document::from(html.as_str());
        let Some(el) = doc.select(next_selector).iter().next() else { break };
        let Some(href) = el.attr("href") else { break };
        let Ok(base) = Url::parse(&url) else { break };
        let Ok(next) = base.join(href.trim()) else { break };
        url = next.to_string();
    }
    Ok(links)
}

fn extension_of(url: &str) -> String {
    let path = Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| url.to_string());
    path.rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn is_media_ext(ext: &str) -> bool {
    matches!(
        ext,
        "mp4" | "mkv"
            | "webm"
            | "avi"
            | "mov"
            | "m3u8"
            | "mpd"
            | "ts"
            | "mp3"
            | "flac"
            | "m4a"
            | "wav"
            | "ogg"
            | "opus"
            | "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "zip"
            | "rar"
            | "7z"
            | "pdf"
            | "apk"
            | "iso"
    )
}
