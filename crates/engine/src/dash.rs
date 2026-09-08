//! MPEG-DASH (mpd) support: manifest parsing, representation (quality)
//! listing, and segment downloading.
//!
//! Parses MPD XML directly (Period → AdaptationSet → Representation →
//! SegmentTemplate). Video representations are exposed as qualities; the
//! chosen representation id is carried back in a `#rep=<id>` fragment of the
//! manifest URL. Segments (init + media) are downloaded in parallel and
//! concatenated into a fragmented MP4.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use reqwest::Client;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

#[derive(Debug, Error)]
pub enum DashError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("no downloadable representation")]
    NoRepresentation,
    #[error("cannot determine segment count (no duration or timeline)")]
    NoSegmentCount,
    #[error("cancelled")]
    Cancelled,
}

/// A downloadable representation of a DASH adaptation set.
#[derive(Debug, Clone)]
pub struct Representation {
    pub id: String,
    pub label: String,
    pub bandwidth: u64,
}

/// Parsed manifest summary used by the quality picker.
#[derive(Debug, Clone)]
pub struct DashManifest {
    pub is_drm: bool,
    /// Total presentation duration in seconds, when advertised.
    pub duration_secs: Option<f64>,
    pub video: Vec<Representation>,
    /// Best audio representation, downloaded and muxed via ffmpeg when present.
    pub audio: Option<Representation>,
}

#[derive(Debug, Default, Clone)]
struct RawRep {
    id: String,
    width: u64,
    height: u64,
    bandwidth: u64,
    content_type: String,
    initialization: String,
    media: String,
    start_number: u64,
    timescale: u64,
    duration: u64,
    /// SegmentTimeline entries (t, d, r).
    timeline: Option<Vec<(Option<u64>, u64, u64)>>,
}

/// Fetches and parses an MPD, listing video representations.
pub async fn fetch_variants(client: &Client, mpd_url: &str) -> Result<DashManifest, DashError> {
    let bytes = client
        .get(mpd_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let text = String::from_utf8_lossy(&bytes);
    parse_manifest(&text)
}

/// Parses the MPD XML into quality listings.
pub fn parse_manifest(xml: &str) -> Result<DashManifest, DashError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut duration_secs: Option<f64> = None;
    let mut is_drm = false;
    let mut reps: Vec<RawRep> = Vec::new();

    // AdaptationSet-level defaults inherited by representations.
    let mut set_content_type = String::new();
    let mut set_tpl: Tpl = Tpl::default();
    let mut in_representation = false;
    let mut current: Option<RawRep> = None;
    let mut in_segment_template = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "MPD" => {
                        for attr in attrs(&e) {
                            if attr.0 == "mediaPresentationDuration" {
                                duration_secs = parse_iso_duration(&attr.1);
                            }
                        }
                    }
                    "AdaptationSet" => {
                        set_content_type.clear();
                        set_tpl = Tpl::default();
                        for attr in attrs(&e) {
                            match attr.0.as_str() {
                                "contentType" => set_content_type = attr.1,
                                "mimeType" if set_content_type.is_empty() => {
                                    set_content_type = attr.1
                                }
                                _ => {}
                            }
                        }
                    }
                    "SegmentTemplate" => {
                        in_segment_template = true;
                        let tpl = read_template_attrs(&e);
                        if in_representation {
                            if let Some(rep) = current.as_mut() {
                                apply_template(rep, &tpl);
                            }
                        } else {
                            set_tpl = tpl;
                        }
                    }
                    "S" if in_segment_template => {
                        let mut t = None;
                        let mut d = 0u64;
                        let mut r = 0u64;
                        for attr in attrs(&e) {
                            match attr.0.as_str() {
                                "t" => t = attr.1.parse().ok(),
                                "d" => d = attr.1.parse().unwrap_or(0),
                                "r" => r = attr.1.parse().unwrap_or(0),
                                _ => {}
                            }
                        }
                        let target = if in_representation {
                            current.as_mut()
                        } else {
                            None
                        };
                        if let Some(rep) = target {
                            rep.timeline.get_or_insert_with(Vec::new).push((t, d, r));
                        } else {
                            set_tpl
                                .timeline
                                .get_or_insert_with(Vec::new)
                                .push((t, d, r));
                        }
                    }
                    "Representation" => {
                        in_representation = true;
                        let mut rep = RawRep {
                            content_type: set_content_type.clone(),
                            initialization: set_tpl.initialization.clone(),
                            media: set_tpl.media.clone(),
                            start_number: set_tpl.start_number,
                            timescale: set_tpl.timescale,
                            duration: set_tpl.duration,
                            timeline: set_tpl.timeline.clone(),
                            ..Default::default()
                        };
                        for attr in attrs(&e) {
                            match attr.0.as_str() {
                                "id" => rep.id = attr.1,
                                "bandwidth" => rep.bandwidth = attr.1.parse().unwrap_or(0),
                                "width" => rep.width = attr.1.parse().unwrap_or(0),
                                "height" => rep.height = attr.1.parse().unwrap_or(0),
                                "mimeType" if rep.content_type.is_empty() => {
                                    rep.content_type = attr.1
                                }
                                _ => {}
                            }
                        }
                        current = Some(rep);
                    }
                    "ContentProtection" => is_drm = true,
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "SegmentTemplate" {
                    let tpl = read_template_attrs(&e);
                    if in_representation {
                        if let Some(rep) = current.as_mut() {
                            apply_template(rep, &tpl);
                        }
                    } else {
                        set_tpl = tpl;
                    }
                } else if name == "ContentProtection" {
                    is_drm = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "Representation" => {
                        in_representation = false;
                        in_segment_template = false;
                        if let Some(rep) = current.take() {
                            reps.push(rep);
                        }
                    }
                    "SegmentTemplate" => in_segment_template = false,
                    "AdaptationSet" => set_tpl = Tpl::default(),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DashError::Manifest(e.to_string())),
            _ => {}
        }
    }

    let mut video: Vec<Representation> = Vec::new();
    let mut best_audio: Option<(u64, Representation)> = None;
    for raw in reps {
        let usable = !raw.media.is_empty() || !raw.initialization.is_empty();
        if !usable || raw.id.is_empty() {
            continue;
        }
        let is_video = raw.content_type.starts_with("video") || raw.width > 0;
        let is_audio = raw.content_type.starts_with("audio");
        if is_video {
            let label = if raw.height > 0 {
                format!("{}p", raw.height)
            } else {
                format!("{} kbps", raw.bandwidth / 1000)
            };
            video.push(Representation {
                id: raw.id,
                label,
                bandwidth: raw.bandwidth,
            });
        } else if is_audio {
            let entry = (
                raw.bandwidth,
                Representation {
                    id: raw.id,
                    label: "audio".into(),
                    bandwidth: raw.bandwidth,
                },
            );
            if best_audio.as_ref().map(|(b, _)| raw.bandwidth > *b).unwrap_or(true) {
                best_audio = Some(entry);
            }
        }
    }
    video.sort_by_key(|r| std::cmp::Reverse(r.bandwidth));
    if video.is_empty() {
        return Err(DashError::NoRepresentation);
    }
    Ok(DashManifest {
        is_drm,
        duration_secs,
        video,
        audio: best_audio.map(|(_, r)| r),
    })
}

#[derive(Debug, Default, Clone)]
struct Tpl {
    initialization: String,
    media: String,
    start_number: u64,
    timescale: u64,
    duration: u64,
    timeline: Option<Vec<(Option<u64>, u64, u64)>>,
}

fn read_template_attrs(e: &BytesStart) -> Tpl {
    let mut tpl = Tpl::default();
    for attr in attrs(e) {
        match attr.0.as_str() {
            "initialization" => tpl.initialization = attr.1,
            "media" => tpl.media = attr.1,
            "startNumber" => tpl.start_number = attr.1.parse().unwrap_or(1),
            "timescale" => tpl.timescale = attr.1.parse().unwrap_or(0),
            "duration" => tpl.duration = attr.1.parse().unwrap_or(0),
            _ => {}
        }
    }
    tpl
}

fn apply_template(rep: &mut RawRep, tpl: &Tpl) {
    if !tpl.initialization.is_empty() {
        rep.initialization = tpl.initialization.clone();
    }
    if !tpl.media.is_empty() {
        rep.media = tpl.media.clone();
    }
    if tpl.start_number > 0 {
        rep.start_number = tpl.start_number;
    }
    if tpl.timescale > 0 {
        rep.timescale = tpl.timescale;
    }
    if tpl.duration > 0 {
        rep.duration = tpl.duration;
    }
    if tpl.timeline.is_some() {
        rep.timeline = tpl.timeline.clone();
    }
}

/// Builds the init segment URL and ordered media segment URLs for one
/// representation. Handles `$Number$`- and `$Time$`-templated media, with or
/// without a SegmentTimeline.
pub fn segment_urls(
    mpd_url: &str,
    xml: &str,
    rep_id: &str,
    duration_secs: Option<f64>,
) -> Result<(String, Vec<String>), DashError> {
    let mut raws = collect_raw_reps(xml);
    raws.retain(|r| r.id == rep_id);
    let raw = raws.into_iter().next().ok_or(DashError::NoRepresentation)?;

    let base = Url::parse(mpd_url).map_err(|e| DashError::Manifest(e.to_string()))?;
    let timescale = if raw.timescale > 0 { raw.timescale } else { 1 };

    // (number, time) pairs for every media segment.
    let mut pairs: Vec<(u64, Option<u64>)> = Vec::new();
    if let Some(entries) = &raw.timeline {
        let mut t: u64 = 0;
        let mut number = if raw.start_number > 0 { raw.start_number } else { 1 };
        for (t0, d, r) in entries {
            let start = t0.unwrap_or(t);
            let count = r + 1;
            for i in 0..count {
                pairs.push((number, Some(start + i * d)));
                number += 1;
            }
            t = start + count * d;
        }
    } else {
        let dur = raw.duration;
        if dur == 0 {
            return Err(DashError::NoSegmentCount);
        }
        let total: u64 = match duration_secs {
            Some(secs) => (secs * timescale as f64).ceil() as u64,
            None => return Err(DashError::NoSegmentCount),
        };
        let count = total.div_ceil(dur);
        if count == 0 || count > 100_000 {
            return Err(DashError::NoSegmentCount);
        }
        let start = if raw.start_number > 0 { raw.start_number } else { 1 };
        for i in 0..count {
            pairs.push((start + i, None));
        }
    }

    let init = resolve_template(&base, &raw.initialization, &raw, 0, None);
    let urls = pairs
        .iter()
        .map(|(n, t)| resolve_template(&base, &raw.media, &raw, *n, *t))
        .collect();
    Ok((init, urls))
}

/// Re-parses raw representation data for URL building.
fn collect_raw_reps(xml: &str) -> Vec<RawRep> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out: Vec<RawRep> = Vec::new();
    let mut set_tpl = Tpl::default();
    let mut in_representation = false;
    let mut current: Option<RawRep> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "SegmentTemplate" => {
                        let tpl = read_template_attrs(&e);
                        if in_representation {
                            if let Some(rep) = current.as_mut() {
                                apply_template(rep, &tpl);
                            }
                        } else {
                            set_tpl = tpl;
                        }
                    }
                    "S" if in_representation => {
                        let mut t = None;
                        let mut d = 0u64;
                        let mut r = 0u64;
                        for attr in attrs(&e) {
                            match attr.0.as_str() {
                                "t" => t = attr.1.parse().ok(),
                                "d" => d = attr.1.parse().unwrap_or(0),
                                "r" => r = attr.1.parse().unwrap_or(0),
                                _ => {}
                            }
                        }
                        if let Some(rep) = current.as_mut() {
                            rep.timeline.get_or_insert_with(Vec::new).push((t, d, r));
                        }
                    }
                    "Representation" => {
                        in_representation = true;
                        let mut rep = RawRep {
                            initialization: set_tpl.initialization.clone(),
                            media: set_tpl.media.clone(),
                            start_number: set_tpl.start_number,
                            timescale: set_tpl.timescale,
                            duration: set_tpl.duration,
                            timeline: set_tpl.timeline.clone(),
                            ..Default::default()
                        };
                        for attr in attrs(&e) {
                            match attr.0.as_str() {
                                "id" => rep.id = attr.1,
                                "bandwidth" => rep.bandwidth = attr.1.parse().unwrap_or(0),
                                "width" => rep.width = attr.1.parse().unwrap_or(0),
                                "height" => rep.height = attr.1.parse().unwrap_or(0),
                                "mimeType" if rep.content_type.is_empty() => {
                                    rep.content_type = attr.1
                                }
                                _ => {}
                            }
                        }
                        current = Some(rep);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if local_name(e.name().as_ref()) == "SegmentTemplate" {
                    let tpl = read_template_attrs(&e);
                    if in_representation {
                        if let Some(rep) = current.as_mut() {
                            apply_template(rep, &tpl);
                        }
                    } else {
                        set_tpl = tpl;
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "Representation" {
                    in_representation = false;
                    if let Some(rep) = current.take() {
                        out.push(rep);
                    }
                } else if name == "AdaptationSet" {
                    set_tpl = Tpl::default();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Substitutes `$RepresentationID$`, `$Bandwidth$`, `$Number[wd]$`,
/// `$Time[wd]$` and resolves against the manifest base URL.
fn resolve_template(
    base: &Url,
    template: &str,
    rep: &RawRep,
    number: u64,
    time: Option<u64>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '$' {
                out.push('$');
                i += 2;
                continue;
            }
            if let Some((name, width, consumed)) = parse_tag(&chars[i..]) {
                match name.as_str() {
                    "RepresentationID" => out.push_str(&rep.id),
                    "Bandwidth" => out.push_str(&rep.bandwidth.to_string()),
                    "Number" => out.push_str(&pad(number, width)),
                    "Time" => out.push_str(&pad(time.unwrap_or(0), width)),
                    _ => {}
                }
                i += consumed;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    base.join(&out)
        .map(|u| u.to_string())
        .unwrap_or(out)
}

/// Recognizes `$Name$` or `$Name%0Nd$` at the start of `chars`; returns
/// (name, zero-pad width, chars consumed).
fn parse_tag(chars: &[char]) -> Option<(String, usize, usize)> {
    let end = chars
        .iter()
        .skip(1)
        .position(|c| *c == '$')?;
    let tag: String = chars[1..1 + end].iter().collect();
    let (name, width) = match tag.split_once('%') {
        Some((n, w)) => {
            let digits = w.strip_prefix('0')?;
            let width = digits.strip_suffix('d')?.parse::<usize>().ok()?;
            (n.to_string(), width)
        }
        None => (tag, 0),
    };
    Some((name, width, end + 2))
}

fn pad(value: u64, width: usize) -> String {
    if width > 1 {
        format!("{:0width$}", value, width = width)
    } else {
        value.to_string()
    }
}

/// Parses ISO8601 durations (`PT1H2M3.5S`).
pub fn parse_iso_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    let body = s.strip_prefix('P')?;
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    fn scan(text: &str, scales: &[(&char, f64)]) -> f64 {
        let mut acc = 0.0;
        let mut num = String::new();
        for c in text.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else if let Some(scale) = scales
                .iter()
                .find(|(k, _)| **k == c)
                .map(|(_, s)| *s)
            {
                acc += num.parse::<f64>().unwrap_or(0.0) * scale;
                num.clear();
            }
        }
        acc
    }
    let mut total = scan(
        date,
        &[
            (&'Y', 365.0 * 86400.0),
            (&'M', 30.0 * 86400.0),
            (&'W', 7.0 * 86400.0),
            (&'D', 86400.0),
        ],
    );
    if let Some(t) = time {
        total += scan(t, &[(&'H', 3600.0), (&'M', 60.0), (&'S', 1.0)]);
    }
    if total > 0.0 { Some(total) } else { None }
}

/// Downloads init + media segments into `dest_dir` as `init.mp4` /
/// `segment_00001.m4s`. Bounded concurrency, cancellation, resume.
pub async fn download_segments(
    client: &Client,
    init_url: &str,
    segment_urls: Vec<String>,
    dest_dir: &Path,
    max_concurrent: usize,
    progress: UnboundedSender<(u64, u64)>,
    cancel: Arc<AtomicBool>,
) -> Result<(), DashError> {
    tokio::fs::create_dir_all(dest_dir).await?;
    let total = segment_urls.len() as u64 + 1; // + init
    let done = Arc::new(AtomicU64::new(0));

    // Init segment (resume-safe).
    let init_path = dest_dir.join("init.mp4");
    if tokio::fs::metadata(&init_path)
        .await
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        done.fetch_add(1, Ordering::Relaxed);
    } else {
        let bytes = client
            .get(init_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        tokio::fs::write(&init_path, &bytes).await?;
        done.fetch_add(1, Ordering::Relaxed);
    }
    let _ = progress.send((done.load(Ordering::Relaxed), total));

    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent.clamp(1, 64)));
    let mut set = tokio::task::JoinSet::new();

    for (i, url) in segment_urls.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let client = client.clone();
        let semaphore = semaphore.clone();
        let url = url.clone();
        let dest_dir = dest_dir.to_path_buf();
        let name = format!("segment_{:05}.m4s", i + 1);
        let done = done.clone();
        let cancel = cancel.clone();

        set.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| DashError::Manifest("semaphore closed".into()))?;
            if cancel.load(Ordering::Relaxed) {
                return Err(DashError::Cancelled);
            }
            let path = dest_dir.join(&name);
            let exists = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len() > 0)
                .unwrap_or(false);
            if !exists {
                let bytes = client
                    .get(&url)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                tokio::fs::write(&path, &bytes).await?;
            }
            done.fetch_add(1, Ordering::Relaxed);
            Ok::<(), DashError>(())
        });
    }

    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {
                let _ = progress.send((done.load(Ordering::Relaxed), total));
            }
            Ok(Err(DashError::Cancelled)) if cancel.load(Ordering::Relaxed) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(DashError::Manifest(format!("worker panic: {e}"))),
        }
        if cancel.load(Ordering::Relaxed) {
            set.abort_all();
            let _ = progress.send((done.load(Ordering::Relaxed), total));
            return Err(DashError::Cancelled);
        }
    }
    let _ = progress.send((done.load(Ordering::Relaxed), total));
    if cancel.load(Ordering::Relaxed) {
        return Err(DashError::Cancelled);
    }
    Ok(())
}

/// Concatenates init + media segments into `out` (a valid fragmented MP4
/// when the representation is fMP4). Blocking — call from `spawn_blocking`.
pub fn concat_segments(dir: &Path, count: usize, out: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(out)?;
    let mut buf = Vec::new();
    let init = dir.join("init.mp4");
    if init.exists() {
        buf = std::fs::read(&init)?;
        file.write_all(&buf)?;
    }
    for i in 0..count {
        buf.clear();
        let path = dir.join(format!("segment_{:05}.m4s", i + 1));
        if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
            buf = std::fs::read(&path)?;
            file.write_all(&buf)?;
        }
    }
    file.flush()?;
    Ok(())
}

fn local_name(qname: &[u8]) -> String {
    let s = String::from_utf8_lossy(qname);
    s.rsplit(':').next().unwrap_or("").to_string()
}

/// Collects the (name, value) pairs of an element, ignoring namespaces.
fn attrs(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .filter_map(|a| a.ok())
        .map(|a| {
            (
                local_name(a.key.as_ref()),
                a.normalized_value().map(|v| v.to_string()).unwrap_or_default(),
            )
        })
        .collect()
}
