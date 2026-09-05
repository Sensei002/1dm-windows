//! HLS (m3u8) support: playlist parsing, segment planning, downloading with
//! AES-128 decryption. DASH (mpd) is planned in [`crate::dash`].

use std::collections::HashMap;
use std::path::Path;

use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
use m3u8_rs::{parse_master_playlist_res, parse_media_playlist_res, KeyMethod};
use reqwest::Client;
use thiserror::Error;
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

/// Downloads all segments (bounded concurrency), decrypting when needed.
/// Segments are written as `segment_00001.ts` etc. in `dest_dir`.
pub async fn download_segments(
    client: &Client,
    playlist: &HlsPlaylist,
    dest_dir: &Path,
    max_concurrent: usize,
) -> Result<(), HlsError> {
    tokio::fs::create_dir_all(dest_dir).await?;

    // Fetch and cache each distinct AES-128 key once.
    let mut key_cache: HashMap<String, [u8; 16]> = HashMap::new();
    for seg in &playlist.segments {
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

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
        max_concurrent.clamp(1, 64),
    ));
    let mut set = tokio::task::JoinSet::new();

    for (i, seg) in playlist.segments.iter().enumerate() {
        let client = client.clone();
        let semaphore = semaphore.clone();
        let key_cache = key_cache.clone();
        let url = seg.url.clone();
        let key = seg.key.clone();
        let dest_dir = dest_dir.to_path_buf();
        let name = format!("segment_{:05}.ts", i + 1);

        set.spawn(async move {
            let _permit = semaphore.acquire_owned().await.map_err(|_| {
                HlsError::Playlist("semaphore closed".into())
            })?;
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
            Ok::<(), HlsError>(())
        });
    }

    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(HlsError::Playlist(format!("worker panic: {e}")));
            }
        }
    }
    Ok(())
}

fn decrypt_segment(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, HlsError> {
    let mut buf = data.to_vec();
    let pt = Aes128Cbc::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| HlsError::Decrypt)?;
    Ok(pt.to_vec())
}