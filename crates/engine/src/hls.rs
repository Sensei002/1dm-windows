//! HLS (m3u8) support: playlist parsing, quality variants, segment
//! downloading with AES-128 decryption, resume and concat. DASH lives in
//! [`crate::dash`].

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
use m3u8_rs::{parse_master_playlist_res, parse_media_playlist_res, KeyMethod};
use reqwest::Client;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

type Aes128Cbc = Decryptor<Aes128>;

#[derive(Debug, Error)]
pub enum HlsError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid playlist: {0}")]
    Playlist(String),
    #[error("segment uses an unsupported encryption method")]
    UnsupportedEncryption,
    #[error("decryption failed")]
    Decrypt,
    #[error("cancelled")]
    Cancelled,
}

/// A single media segment and how to decrypt it, if at all.
#[derive(Debug, Clone)]
pub struct Segment {
    pub url: String,
    pub key: Option<SegmentKey>,
}

#[derive(Debug, Clone)]
pub struct SegmentKey {
    pub key_url: String,
    pub iv: [u8; 16],
}

#[derive(Debug)]
pub struct HlsPlaylist {
    pub segments: Vec<Segment>,
    pub is_live: bool,
}

/// A selectable quality variant of a master playlist.
#[derive(Debug, Clone)]
pub struct Variant {
    pub label: String,
    pub url: String,
}

/// Master playlist listing with DRM detection, for the quality picker.
#[derive(Debug)]
pub struct HlsVariants {
    pub is_drm: bool,
    pub variants: Vec<Variant>,
}

/// Fetches a playlist URL and lists its quality variants. Detects
/// SAMPLE-AES (DRM) playlists from the raw text.
pub async fn fetch_variants(client: &Client, playlist_url: &str) -> Result<HlsVariants, HlsError> {
    let bytes = client
        .get(playlist_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let is_drm = raw_is_drm(&bytes);

    if parse_media_playlist_res(&bytes).is_ok() {
        return Ok(HlsVariants {
            is_drm,
            variants: vec![Variant {
                label: "source".into(),
                url: playlist_url.to_string(),
            }],
        });
    }

    let master = parse_master_playlist_res(&bytes)
        .map_err(|e| HlsError::Playlist(format!("{e:?}")))?;
    let base = Url::parse(playlist_url).map_err(|e| HlsError::Playlist(e.to_string()))?;
    let mut variants = Vec::new();
    for v in &master.variants {
        let url = base
            .join(&v.uri)
            .map_err(|e| HlsError::Playlist(e.to_string()))?
            .to_string();
        let label = match v.resolution {
            Some((_, h)) if h > 0 => format!("{h}p"),
            _ => format!("{} kbps", v.bandwidth / 1000),
        };
        variants.push(Variant { label, url });
    }
    if variants.is_empty() {
        return Err(HlsError::Playlist("master playlist has no variants".into()));
    }
    variants.sort_by_key(|v| std::cmp::Reverse(variant_height(&v.label)));
    Ok(HlsVariants { is_drm, variants })
}

/// SAMPLE-AES means DRM (Widevine/PlayReady/FairPlay); AES-128 is plain HLS
/// encryption and is downloadable.
fn raw_is_drm(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.to_uppercase().contains("METHOD=SAMPLE-AES")
}

fn variant_height(label: &str) -> u64 {
    label
        .strip_suffix('p')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Fetches a playlist (following master → best variant) and resolves every
/// segment URL + encryption key into a flat, download-ready list.
pub async fn fetch_playlist(
    client: &Client,
    playlist_url: &str,
) -> Result<HlsPlaylist, HlsError> {
    let base = Url::parse(playlist_url).map_err(|e| HlsError::Playlist(e.to_string()))?;
    let bytes = client
        .get(playlist_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // Media playlist?
    if let Ok(media) = parse_media_playlist_res(&bytes) {
        return build_media_playlist(base, media);
    }

    // Master playlist → follow the variant with the highest bandwidth.
    let master = parse_master_playlist_res(&bytes)
        .map_err(|e| HlsError::Playlist(format!("{e:?}")))?;
    let best = master
        .variants
        .iter()
        .max_by_key(|v| v.bandwidth)
        .ok_or_else(|| HlsError::Playlist("master playlist has no variants".into()))?;
    let variant_url = base
        .join(&best.uri)
        .map_err(|e| HlsError::Playlist(e.to_string()))?;
    let bytes = client
        .get(variant_url.as_str())
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let media = parse_media_playlist_res(&bytes)
        .map_err(|e| HlsError::Playlist(format!("{e:?}")))?;
    build_media_playlist(variant_url, media)
}

fn build_media_playlist(base: Url, pl: m3u8_rs::MediaPlaylist) -> Result<HlsPlaylist, HlsError> {
    let mut segments = Vec::with_capacity(pl.segments.len());
    let mut seg_index: u64 = pl.media_sequence;

    for seg in &pl.segments {
        let url = base
            .join(&seg.uri)
            .map_err(|e| HlsError::Playlist(e.to_string()))?
            .to_string();
        let key = match &seg.key {
            Some(k) if matches!(k.method, KeyMethod::AES128) => {
                let key_url = k
                    .uri
                    .as_ref()
                    .ok_or(HlsError::UnsupportedEncryption)?;
                let key_url = base
                    .join(key_url)
                    .map_err(|e| HlsError::Playlist(e.to_string()))?
                    .to_string();
                let iv = parse_iv(k.iv.as_deref(), seg_index)?;
                Some(SegmentKey { key_url, iv })
            }
            Some(_) => return Err(HlsError::UnsupportedEncryption),
            None => None,
        };
        segments.push(Segment { url, key });
        seg_index += 1;
    }

    Ok(HlsPlaylist {
        segments,
        is_live: !pl.end_list,
    })
}

/// IV from the playlist `IV` attribute, or the media sequence (16-byte BE).
fn parse_iv(raw: Option<&str>, media_sequence: u64) -> Result<[u8; 16], HlsError> {
    let raw = raw.unwrap_or("");
    let raw = raw.strip_prefix("0x").unwrap_or(raw);
    if !raw.is_empty() {
        let bytes = hex::decode(raw).map_err(|_| HlsError::Playlist("bad IV".into()))?;
        if bytes.len() == 16 {
            let mut iv = [0u8; 16];
            iv.copy_from_slice(&bytes);
            return Ok(iv);
        }
    }
    // RFC 8216: default IV is the media sequence number as a 128-bit
    // big-endian value, i.e. 8 zero bytes followed by the sequence number.
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&media_sequence.to_be_bytes());
    Ok(iv)
}

/// Segment filename used inside `dest_dir` for index `i` (0-based).
pub fn segment_name(i: usize) -> String {
    format!("segment_{:05}.ts", i + 1)
}

/// Downloads all segments (bounded concurrency), decrypting when needed.
/// Segments are written as `segment_00001.ts` etc. in `dest_dir`. Existing
/// non-empty segment files are skipped, so an interrupted download resumes
/// where it stopped. Progress `(done, total)` counts segments.
pub async fn download_segments(
    client: &Client,
    playlist: &HlsPlaylist,
    dest_dir: &Path,
    max_concurrent: usize,
    progress: UnboundedSender<(u64, u64)>,
    cancel: Arc<AtomicBool>,
) -> Result<(), HlsError> {
    tokio::fs::create_dir_all(dest_dir).await?;
    let total = playlist.segments.len();
    if total == 0 {
        return Err(HlsError::Playlist("playlist has no segments".into()));
    }

    // Fetch and cache each distinct AES-128 key once.
    let mut key_cache: HashMap<String, [u8; 16]> = HashMap::new();
    for seg in &playlist.segments {
        if cancel.load(Ordering::Relaxed) {
            return Err(HlsError::Cancelled);
        }
        if let Some(key) = &seg.key {
            if !key_cache.contains_key(&key.key_url) {
                let bytes = client
                    .get(&key.key_url)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                if bytes.len() != 16 {
                    return Err(HlsError::Playlist("key is not 16 bytes".into()));
                }
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&bytes);
                key_cache.insert(key.key_url.clone(), arr);
            }
        }
    }

    // Resume: count segments that already exist on disk.
    let mut done = Arc::new(AtomicU64::new(0));
    let mut pending: Vec<(usize, &Segment)> = Vec::with_capacity(total);
    for (i, seg) in playlist.segments.iter().enumerate() {
        let path = dest_dir.join(segment_name(i));
        let exists = tokio::fs::metadata(&path)
            .await
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        if exists {
            done.fetch_add(1, Ordering::Relaxed);
        } else {
            pending.push((i, seg));
        }
    }
    let _ = progress.send((done.load(Ordering::Relaxed), total as u64));

    let semaphore = Arc::new(tokio::sync::Semaphore::new(
        max_concurrent.clamp(1, 64),
    ));
    let mut set = tokio::task::JoinSet::new();

    for (i, seg) in pending {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let client = client.clone();
        let semaphore = semaphore.clone();
        let key_cache = key_cache.clone();
        let url = seg.url.clone();
        let key = seg.key.clone();
        let dest_dir = dest_dir.to_path_buf();
        let name = segment_name(i);
        let done = done.clone();
        let cancel = cancel.clone();

        set.spawn(async move {
            let _permit = semaphore.acquire_owned().await.map_err(|_| {
                HlsError::Playlist("semaphore closed".into())
            })?;
            if cancel.load(Ordering::Relaxed) {
                return Err(HlsError::Cancelled);
            }
            let bytes = client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            let data = match &key {
                Some(k) => {
                    let key_bytes = key_cache
                        .get(&k.key_url)
                        .ok_or_else(|| HlsError::Playlist("key not cached".into()))?;
                    decrypt_segment(&bytes, key_bytes, &k.iv)?
                }
                None => bytes.to_vec(),
            };
            tokio::fs::write(dest_dir.join(&name), data).await?;
            done.fetch_add(1, Ordering::Relaxed);
            Ok::<(), HlsError>(())
        });
    }

    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {
                let _ = progress.send((done.load(Ordering::Relaxed), total as u64));
            }
            Ok(Err(e)) if cancel.load(Ordering::Relaxed) => {
                // paused/cancelled: worker failed or aborted mid-flight
            }
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(HlsError::Playlist(format!("worker panic: {e}")));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            set.abort_all();
            let _ = progress.send((done.load(Ordering::Relaxed), total as u64));
            return Err(HlsError::Cancelled);
        }
    }
    let _ = progress.send((done.load(Ordering::Relaxed), total as u64));
    if cancel.load(Ordering::Relaxed) {
        return Err(HlsError::Cancelled);
    }
    Ok(())
}

/// Concatenates downloaded segment files in order into `out` (a valid MPEG-TS
/// stream). Blocking I/O — call from `spawn_blocking`.
pub fn concat_segments(dir: &Path, count: usize, out: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::create(out)?;
    let mut buf = Vec::new();
    for i in 0..count {
        buf.clear();
        let path = dir.join(segment_name(i));
        if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
            buf = std::fs::read(&path)?;
            file.write_all(&buf)?;
        }
    }
    file.flush()?;
    Ok(())
}

fn decrypt_segment(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, HlsError> {
    let mut buf = data.to_vec();
    let pt = Aes128Cbc::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| HlsError::Decrypt)?;
    Ok(pt.to_vec())
}
