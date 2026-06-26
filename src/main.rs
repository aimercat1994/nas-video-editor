use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader},
    process::Command,
    sync::RwLock,
};
use tower_http::services::ServeDir;
use tracing::{error, info, warn};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// =========================================================================
// Config
// =========================================================================

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg",
    "ts", "vob", "3gp", "ogv",
];

const ALLOWED_CODECS: &[&str] = &[
    "copy", "libx264", "libx265", "h264_nvenc", "hevc_nvenc", "h264_qsv",
    "hevc_qsv", "h264_vaapi", "hevc_vaapi",
];

const ALLOWED_FORMATS: &[&str] = &["mp4", "mkv", "webm", "avi", "mov"];

const BLOCKED_ARGS: &[&str] = &[
    "-i", "-f", "-map", "-script", "-protocol_whitelist", "-protocol_blacklist",
    "-sdp_file", "-hls_key_info_file", "-dash_segment_filename",
];

const BLOCKED_PATTERNS: &[&str] = &[
    "file=", "filename=", "drawtext", "readtext", "pipe:", "http:", "rtmp:", "tcp:", "udp:",
];

// =========================================================================
// State
// =========================================================================

#[derive(Clone)]
struct AppState {
    videos_dir: PathBuf,
    frontend_dir: PathBuf,
    password: String,
    secret: String,
    tasks: Arc<RwLock<HashMap<String, Task>>>,
}

#[derive(Clone, Serialize)]
struct Task {
    id: String,
    #[serde(rename = "type")]
    task_type: String,
    source: String,
    status: String,
    progress: f64,
    message: String,
    output: Option<String>,
    error: Option<String>,
    created_at: String,
}

// =========================================================================
// Request/Response Models
// =========================================================================

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

#[derive(Deserialize)]
struct CutRequest {
    file: String,
    start: f64,
    end: f64,
    #[serde(default = "default_codec")]
    codec: String,
    resolution: Option<String>,
    format: Option<String>,
    extra_args: Option<String>,
}

#[derive(Deserialize)]
struct ConcatRequest {
    file: String,
    segments: Vec<Segment>,
    #[serde(default = "default_codec")]
    codec: String,
    resolution: Option<String>,
    format: Option<String>,
    extra_args: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Segment {
    start: f64,
    end: f64,
}

#[derive(Deserialize)]
struct FilesQuery {
    path: Option<String>,
}

fn default_codec() -> String {
    "copy".to_string()
}

// =========================================================================
// Helpers
// =========================================================================

fn fmt_time(seconds: f64) -> String {
    let h = (seconds / 3600.0).floor() as u64;
    let m = ((seconds % 3600.0) / 60.0).floor() as u64;
    let s = seconds % 60.0;
    format!("{:02}:{:02}:{:06.3}", h, m, s)
}

fn fmt_size(bytes: u64) -> String {
    let mut size = bytes as f64;
    for unit in &["B", "KB", "MB", "GB", "TB"] {
        if size < 1024.0 {
            return format!("{:.1} {}", size, unit);
        }
        size /= 1024.0;
    }
    format!("{:.1} PB", size)
}

fn safe_path(videos_dir: &Path, rel: &str) -> Result<PathBuf, StatusCode> {
    if rel.contains('\0') || rel.len() > 1024 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let p = videos_dir.join(rel).canonicalize().unwrap_or_default();
    let base = videos_dir.canonicalize().unwrap_or_default();
    if !p.starts_with(&base) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(p)
}

fn make_output_path(source: &Path, suffix: &str, ext: Option<&str>) -> PathBuf {
    let ts = Utc::now().format("%Y%m%d_%H%M%S");
    let out_ext = ext.unwrap_or(
        source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4"),
    );
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = source.parent().unwrap_or(Path::new("."));
    parent.join(format!("{}_{}_{}{}", stem, suffix, ts, out_ext))
}

fn sanitize_extra_args(args: &str) -> Vec<String> {
    let mut safe = Vec::new();
    let mut skip_next = false;
    for part in args.split_whitespace() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if BLOCKED_ARGS.contains(&part) {
            skip_next = true;
            continue;
        }
        let lower = part.to_lowercase();
        if BLOCKED_PATTERNS.iter().any(|p| lower.contains(p)) {
            continue;
        }
        safe.push(part.to_string());
    }
    safe
}

fn validate_resolution(resolution: &str) -> Result<(u32, u32), StatusCode> {
    let parts: Vec<&str> = resolution.split('x').collect();
    if parts.len() != 2 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let w: u32 = parts[0].parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let h: u32 = parts[1].parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if w < 16 || h < 16 || w > 7680 || h > 4320 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((w, h))
}

fn encoder_extra_args(codec: &str, resolution: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(res) = resolution {
        if let Ok((w, h)) = validate_resolution(res) {
            args.push("-vf".to_string());
            args.push(format!(
                "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2",
                w, h, w, h
            ));
        }
    }
    let _ = codec; // codec used by caller
    args
}

// =========================================================================
// Auth
// =========================================================================

fn make_token(password: &str, secret: &str) -> String {
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 7 * 86400;
    let payload = format!("{}:{}", password, expiry);
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    format!("{}:{}", expiry, sig)
}

fn verify_token(token: &str, password: &str, secret: &str) -> bool {
    let parts: Vec<&str> = token.splitn(2, ':').collect();
    if parts.len() != 2 {
        return false;
    }
    let expiry: u64 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig = parts[1];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now > expiry {
        return false;
    }
    let payload = format!("{}:{}", password, expiry);
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    // Constant-time comparison
    sig.len() == expected.len()
        && sig
            .bytes()
            .zip(expected.bytes())
            .fold(0, |acc, (a, b)| acc | (a ^ b))
            == 0
}

// =========================================================================
// Auth middleware
// =========================================================================

async fn auth_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let exempt = ["/api/login", "/api/auth/check", "/api/logout"];

    if path.starts_with("/api/") && !exempt.iter().any(|e| path == *e) && !state.password.is_empty()
    {
        let token = extract_cookie(&request, "auth_token").unwrap_or_default();
        if !verify_token(&token, &state.password, &state.secret) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"detail": "Not authenticated"})),
            )
                .into_response();
        }
    }

    next.run(request).await
}

fn extract_cookie(req: &Request, name: &str) -> Option<String> {
    let cookie_header = req.headers().get("cookie")?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(name).and_then(|s| s.strip_prefix('=')) {
            return Some(val.to_string());
        }
    }
    None
}

// =========================================================================
// Handlers — Auth
// =========================================================================

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if !state.password.is_empty() && req.password != state.password {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"detail": "Wrong password"})),
        )
            .into_response();
    }

    let token = make_token(&state.password, &state.secret);
    let cookie = format!(
        "auth_token={}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
        token,
        7 * 86400
    );

    let mut resp = Json(serde_json::json!({"ok": true})).into_response();
    resp.headers_mut()
        .insert("set-cookie", cookie.parse().unwrap());
    resp
}

async fn auth_check(State(state): State<AppState>, req: Request) -> Response {
    if state.password.is_empty() {
        return Json(serde_json::json!({"authenticated": true, "no_password": true})).into_response();
    }
    let token = extract_cookie(&req, "auth_token").unwrap_or_default();
    if verify_token(&token, &state.password, &state.secret) {
        return Json(serde_json::json!({"authenticated": true})).into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"detail": "Not authenticated"})),
    )
        .into_response()
}

async fn logout() -> Response {
    let mut resp = Json(serde_json::json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        "set-cookie",
        "auth_token=; Max-Age=0; Path=/; HttpOnly".parse().unwrap(),
    );
    resp
}

// =========================================================================
// Handlers — GPU Detection
// =========================================================================

async fn detect_gpu() -> Json<serde_json::Value> {
    let mut available = vec!["copy", "libx264", "libx265"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();

    let gpu_encoders = [
        "h264_nvenc",
        "hevc_nvenc",
        "h264_qsv",
        "hevc_qsv",
        "h264_vaapi",
        "hevc_vaapi",
    ];

    for enc in &gpu_encoders {
        let ok = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=256x256:d=0.1",
                "-c:v",
                enc,
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            available.push(enc.to_string());
        }
    }

    let recommended = if available.len() > 3 {
        available.last().unwrap().clone()
    } else {
        "copy".to_string()
    };

    Json(serde_json::json!({
        "available": available,
        "recommended": recommended
    }))
}

// =========================================================================
// Handlers — File Browsing
// =========================================================================

async fn list_files(
    State(state): State<AppState>,
    Query(q): Query<FilesQuery>,
) -> Response {
    let path = q.path.unwrap_or_default();
    let target = if path.is_empty() {
        state.videos_dir.clone()
    } else {
        match safe_path(&state.videos_dir, &path) {
            Ok(p) => p,
            Err(s) => return (s, "Invalid path").into_response(),
        }
    };

    if !target.is_dir() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Directory not found"})),
        )
            .into_response();
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    let mut entries = match fs::read_dir(&target).await {
        Ok(e) => e,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"detail": "Cannot read directory"})),
            )
                .into_response()
        }
    };

    let mut all_entries = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        all_entries.push(entry);
    }
    all_entries.sort_by_key(|e| e.file_name());

    for entry in all_entries {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let rel = entry
            .path()
            .strip_prefix(&state.videos_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let ft = match entry.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_dir() {
            dirs.push(serde_json::json!({
                "name": file_name,
                "path": rel,
                "type": "directory"
            }));
        } else if ft.is_file() {
            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
                let meta = entry.metadata().await.ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                files.push(serde_json::json!({
                    "name": file_name,
                    "path": rel,
                    "type": "file",
                    "size": size,
                    "size_fmt": fmt_size(size),
                }));
            }
        }
    }

    Json(serde_json::json!({
        "current": if path.is_empty() { "/" } else { &path },
        "parent": if path.is_empty() { None } else { Some(Path::new(&path).parent().unwrap_or(Path::new("")).to_string_lossy().to_string()) },
        "dirs": dirs,
        "files": files,
    }))
    .into_response()
}

// =========================================================================
// Handlers — Video Streaming
// =========================================================================

async fn stream_video(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    req: Request,
) -> Response {
    let fp = match safe_path(&state.videos_dir, &path) {
        Ok(p) => p,
        Err(s) => return (s, "Invalid path").into_response(),
    };

    if !fp.is_file() {
        return (StatusCode::NOT_FOUND, "Video not found").into_response();
    }

    let file_size = match fs::metadata(&fp).await {
        Ok(m) => m.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot read file").into_response(),
    };

    let content_type = mime_guess::from_path(&fp)
        .first_or_octet_stream()
        .to_string();

    // Check for Range header
    if let Some(range) = req.headers().get("range").and_then(|v| v.to_str().ok()) {
        let re = Regex::new(r"bytes=(\d+)-(\d*)").unwrap();
        if let Some(caps) = re.captures(range) {
            let start: u64 = caps[1].parse().unwrap_or(0);
            let end: u64 = caps[2]
                .parse()
                .unwrap_or(0)
                .max(start + 10 * 1024 * 1024)
                .min(file_size - 1);
            let length = end - start + 1;

            let file = match fs::File::open(&fp).await {
                Ok(f) => f,
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot open file").into_response()
                }
            };

            let stream = async_stream::stream! {
                let mut reader = BufReader::new(file);
                if reader.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                    return;
                }
                let mut remaining = length;
                let mut buf = vec![0u8; 1024 * 1024];
                while remaining > 0 {
                    let to_read = buf.len().min(remaining as usize);
                    match reader.read(&mut buf[..to_read]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            remaining -= n as u64;
                            yield Ok::<_, std::io::Error>(buf[..n].to_vec());
                        }
                        Err(_) => break,
                    }
                }
            };

            return Response::builder()
                .status(206)
                .header(header::CONTENT_TYPE, content_type)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", start, end, file_size),
                )
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, length.to_string())
                .body(Body::from_stream(stream))
                .unwrap()
                .into_response();
        }
    }

    // No range — serve full file
    let file = match fs::File::open(&fp).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot open file").into_response(),
    };

    let stream = async_stream::stream! {
        let mut reader = BufReader::new(file);
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<_, std::io::Error>(buf[..n].to_vec()),
                Err(_) => break,
            }
        }
    };

    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, file_size.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response()
}

// =========================================================================
// Handlers — Video Info (ffprobe)
// =========================================================================

async fn video_info(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let fp = match safe_path(&state.videos_dir, &path) {
        Ok(p) => p,
        Err(s) => return (s, "Invalid path").into_response(),
    };

    if !fp.is_file() {
        return (StatusCode::NOT_FOUND, "Video not found").into_response();
    }

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            fp.to_str().unwrap_or(""),
        ])
        .output()
        .await;

    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"detail": "ffprobe failed"})),
            )
                .into_response()
        }
    };

    let data: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(d) => d,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"detail": "ffprobe parse error"})),
            )
                .into_response()
        }
    };

    let fmt = data.get("format").unwrap_or(&serde_json::Value::Null);
    let streams = data
        .get("streams")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    let video_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let audio_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let file_name = fp
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let rel_path = fp
        .strip_prefix(&state.videos_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let duration = fmt
        .get("duration")
        .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or(v.as_f64()))
        .unwrap_or(0.0);
    let size = fmt
        .get("size")
        .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or(v.as_u64()))
        .unwrap_or(0);
    let format_name = fmt
        .get("format_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Json(serde_json::json!({
        "filename": file_name,
        "path": rel_path,
        "duration": duration,
        "size": size,
        "size_fmt": fmt_size(size),
        "format": format_name,
        "video": {
            "codec_name": video_stream.get("codec_name").and_then(|v| v.as_str()).unwrap_or(""),
            "width": video_stream.get("width").and_then(|v| v.as_u64()).unwrap_or(0),
            "height": video_stream.get("height").and_then(|v| v.as_u64()).unwrap_or(0),
            "fps": video_stream.get("r_frame_rate").and_then(|v| v.as_str()).unwrap_or(""),
            "bitrate": video_stream.get("bit_rate").and_then(|v| v.as_str().map(|s| s.to_string()).or(v.as_u64().map(|n| n.to_string()))).unwrap_or_default(),
        },
        "audio": {
            "codec_name": audio_stream.get("codec_name").and_then(|v| v.as_str()).unwrap_or(""),
            "channels": audio_stream.get("channels").and_then(|v| v.as_u64()).unwrap_or(0),
            "sample_rate": audio_stream.get("sample_rate").and_then(|v| v.as_str().map(|s| s.to_string()).or(v.as_u64().map(|n| n.to_string()))).unwrap_or_default(),
        },
    }))
    .into_response()
}

// =========================================================================
// Handlers — Cut
// =========================================================================

async fn cut_video(
    State(state): State<AppState>,
    Json(req): Json<CutRequest>,
) -> Response {
    let src = match safe_path(&state.videos_dir, &req.file) {
        Ok(p) => p,
        Err(s) => return (s, "Invalid path").into_response(),
    };

    if !src.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Source not found"})),
        )
            .into_response();
    }

    if req.start >= req.end {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "start must be < end"})),
        )
            .into_response();
    }

    if !ALLOWED_CODECS.contains(&req.codec.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "Unsupported codec"})),
        )
            .into_response();
    }

    if let Some(ref fmt) = req.format {
        if !ALLOWED_FORMATS.contains(&fmt.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "Unsupported format"})),
            )
                .into_response();
        }
    }

    if let Some(ref res) = req.resolution {
        if validate_resolution(res).is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "Invalid resolution"})),
            )
                .into_response();
        }
    }

    // Cleanup old tasks
    cleanup_tasks(&state.tasks).await;

    let tasks = state.tasks.read().await;
    if tasks.len() >= 100 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"detail": "Too many tasks"})),
        )
            .into_response();
    }
    drop(tasks);

    let task_id = Uuid::new_v4().to_string()[..8].to_string();
    let out_ext = req.format.as_deref();
    let out_path = make_output_path(&src, "CLIP", out_ext);
    let output_rel = out_path
        .strip_prefix(&state.videos_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let is_copy = req.codec == "copy";

    let mut cmd = vec![
        "ffmpeg".to_string(),
        "-hide_banner".to_string(),
        "-y".to_string(),
    ];

    if is_copy {
        cmd.extend([
            "-ss".to_string(),
            fmt_time(req.start),
            "-i".to_string(),
            src.to_string_lossy().to_string(),
            "-t".to_string(),
            (req.end - req.start).to_string(),
            "-c".to_string(),
            "copy".to_string(),
            "-avoid_negative_ts".to_string(),
            "make_zero".to_string(),
        ]);
    } else {
        cmd.extend([
            "-i".to_string(),
            src.to_string_lossy().to_string(),
            "-ss".to_string(),
            fmt_time(req.start),
            "-to".to_string(),
            fmt_time(req.end),
            "-c:v".to_string(),
            req.codec.clone(),
        ]);
        cmd.extend(encoder_extra_args(&req.codec, req.resolution.as_deref()));
        cmd.extend(["-c:a".to_string(), "aac".to_string(), "-b:a".to_string(), "128k".to_string()]);
        cmd.extend(["-movflags".to_string(), "faststart".to_string()]);
    }

    if let Some(ref extra) = req.extra_args {
        cmd.extend(sanitize_extra_args(extra));
    }

    cmd.push(out_path.to_string_lossy().to_string());

    let total_duration = req.end - req.start;

    // Create task
    let task = Task {
        id: task_id.clone(),
        task_type: "cut".to_string(),
        source: req.file,
        status: "queued".to_string(),
        progress: 0.0,
        message: "Starting FFmpeg...".to_string(),
        output: Some(output_rel),
        error: None,
        created_at: Utc::now().to_rfc3339(),
    };

    {
        let mut tasks = state.tasks.write().await;
        tasks.insert(task_id.clone(), task);
    }

    // Spawn FFmpeg
    let tasks_clone = state.tasks.clone();
    let tid = task_id.clone();
    tokio::spawn(async move {
        run_ffmpeg(tasks_clone, tid, cmd, total_duration).await;
    });

    Json(serde_json::json!({
        "task_id": task_id,
        "output": out_path.strip_prefix(&state.videos_dir).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
    }))
    .into_response()
}

// =========================================================================
// Handlers — Concat
// =========================================================================

async fn concat_segments(
    State(state): State<AppState>,
    Json(req): Json<ConcatRequest>,
) -> Response {
    let src = match safe_path(&state.videos_dir, &req.file) {
        Ok(p) => p,
        Err(s) => return (s, "Invalid path").into_response(),
    };

    if !src.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Source not found"})),
        )
            .into_response();
    }

    if req.segments.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "Need at least 1 segment"})),
        )
            .into_response();
    }

    if !ALLOWED_CODECS.contains(&req.codec.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "Unsupported codec"})),
        )
            .into_response();
    }

    if let Some(ref fmt) = req.format {
        if !ALLOWED_FORMATS.contains(&fmt.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "Unsupported format"})),
            )
                .into_response();
        }
    }

    if let Some(ref res) = req.resolution {
        if validate_resolution(res).is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "Invalid resolution"})),
            )
                .into_response();
        }
    }

    cleanup_tasks(&state.tasks).await;

    let tasks = state.tasks.read().await;
    if tasks.len() >= 100 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"detail": "Too many tasks"})),
        )
            .into_response();
    }
    drop(tasks);

    let task_id = Uuid::new_v4().to_string()[..8].to_string();
    let out_ext = req.format.as_deref();
    let out_path = make_output_path(&src, "CONCAT", out_ext);
    let output_rel = out_path
        .strip_prefix(&state.videos_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let is_copy = req.codec == "copy";
    let total_duration: f64 = req.segments.iter().map(|s| s.end - s.start).sum();

    let mut cmd = vec![
        "ffmpeg".to_string(),
        "-hide_banner".to_string(),
        "-y".to_string(),
    ];

    if is_copy && req.segments.len() == 1 {
        let seg = &req.segments[0];
        cmd.extend([
            "-ss".to_string(),
            fmt_time(seg.start),
            "-i".to_string(),
            src.to_string_lossy().to_string(),
            "-t".to_string(),
            (seg.end - seg.start).to_string(),
            "-c".to_string(),
            "copy".to_string(),
            "-avoid_negative_ts".to_string(),
            "make_zero".to_string(),
        ]);
    } else {
        // Multi-segment: filter_complex
        for _ in &req.segments {
            cmd.extend(["-i".to_string(), src.to_string_lossy().to_string()]);
        }

        let mut filter_parts = Vec::new();
        for (i, seg) in req.segments.iter().enumerate() {
            let mut vfilter = format!(
                "trim=start={}:end={},setpts=PTS-STARTPTS",
                seg.start, seg.end
            );
            if let Some(ref res) = req.resolution {
                if let Ok((w, h)) = validate_resolution(res) {
                    vfilter += &format!(",scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2", w, h, w, h);
                }
            }
            let afilter = format!(
                "atrim=start={}:end={},asetpts=N/SR/TB",
                seg.start, seg.end
            );
            filter_parts.push(format!("[{}:v]{}[v{}];[{}:a]{}[a{}]", i, vfilter, i, i, afilter, i));
        }

        let concat_inputs: String = (0..req.segments.len())
            .map(|i| format!("[v{}][a{}]", i, i))
            .collect();
        filter_parts.push(format!(
            "{}concat=n={}:v=1:a=1[outv][outa]",
            concat_inputs,
            req.segments.len()
        ));

        cmd.extend(["-filter_complex".to_string(), filter_parts.join(";")]);
        cmd.extend(["-map".to_string(), "[outv]".to_string()]);
        cmd.extend(["-map".to_string(), "[outa]".to_string()]);

        if is_copy {
            cmd.extend(["-c:v".to_string(), "libx264".to_string()]);
        } else {
            cmd.extend(["-c:v".to_string(), req.codec.clone()]);
        }
        cmd.extend(["-c:a".to_string(), "aac".to_string(), "-b:a".to_string(), "128k".to_string()]);
        cmd.extend(["-movflags".to_string(), "faststart".to_string()]);
    }

    if let Some(ref extra) = req.extra_args {
        cmd.extend(sanitize_extra_args(extra));
    }

    cmd.push(out_path.to_string_lossy().to_string());

    let task = Task {
        id: task_id.clone(),
        task_type: "concat".to_string(),
        source: req.file,
        status: "queued".to_string(),
        progress: 0.0,
        message: "Starting FFmpeg...".to_string(),
        output: Some(output_rel),
        error: None,
        created_at: Utc::now().to_rfc3339(),
    };

    {
        let mut tasks = state.tasks.write().await;
        tasks.insert(task_id.clone(), task);
    }

    let tasks_clone = state.tasks.clone();
    let tid = task_id.clone();
    tokio::spawn(async move {
        run_ffmpeg(tasks_clone, tid, cmd, total_duration).await;
    });

    Json(serde_json::json!({
        "task_id": task_id,
        "output": out_path.strip_prefix(&state.videos_dir).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
    }))
    .into_response()
}

// =========================================================================
// Handlers — Faststart
// =========================================================================

async fn faststart_video(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let fp = match safe_path(&state.videos_dir, &path) {
        Ok(p) => p,
        Err(s) => return (s, "Invalid path").into_response(),
    };

    if !fp.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Video not found"})),
        )
            .into_response();
    }

    let ext = fp
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !["mp4", "m4v", "mov"].contains(&ext.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"detail": "Only MP4/M4V/MOV files need faststart"})),
        )
            .into_response();
    }

    let task_id = Uuid::new_v4().to_string()[..8].to_string();
    let tmp_path = fp.parent().unwrap().join(format!(
        ".{}_faststart_tmp.{}",
        fp.file_stem().unwrap().to_string_lossy(),
        ext
    ));

    let task = Task {
        id: task_id.clone(),
        task_type: "faststart".to_string(),
        source: path,
        status: "running".to_string(),
        progress: 0.0,
        message: "正在优化 MP4 结构...".to_string(),
        output: None,
        error: None,
        created_at: Utc::now().to_rfc3339(),
    };

    {
        let mut tasks = state.tasks.write().await;
        tasks.insert(task_id.clone(), task);
    }

    let tasks_clone = state.tasks.clone();
    let tid = task_id.clone();
    let fp_clone = fp.clone();
    let tmp_clone = tmp_path.clone();

    tokio::spawn(async move {
        let result = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-y",
                "-i",
                fp_clone.to_str().unwrap_or(""),
                "-c",
                "copy",
                "-movflags",
                "faststart",
                tmp_clone.to_str().unwrap_or(""),
            ])
            .output()
            .await;

        let mut tasks = tasks_clone.write().await;
        if let Some(task) = tasks.get_mut(&tid) {
            match result {
                Ok(o) if o.status.success() && tmp_clone.is_file() => {
                    let _ = fs::rename(&tmp_clone, &fp_clone).await;
                    task.status = "completed".to_string();
                    task.progress = 1.0;
                    task.message = format!(
                        "优化完成: {}",
                        fp_clone.file_name().unwrap().to_string_lossy()
                    );
                }
                Ok(o) => {
                    task.status = "failed".to_string();
                    task.error = Some(String::from_utf8_lossy(&o.stderr).to_string());
                    task.message = "FFmpeg faststart 失败".to_string();
                }
                Err(e) => {
                    task.status = "failed".to_string();
                    task.error = Some(e.to_string());
                    task.message = format!("错误: {}", e);
                }
            }
        }
        let _ = fs::remove_file(&tmp_clone).await;
    });

    Json(serde_json::json!({"task_id": task_id})).into_response()
}

// =========================================================================
// FFmpeg runner
// =========================================================================

async fn run_ffmpeg(
    tasks: Arc<RwLock<HashMap<String, Task>>>,
    task_id: String,
    cmd: Vec<String>,
    total_duration: f64,
) {
    // Update status to running
    {
        let mut tasks = tasks.write().await;
        if let Some(task) = tasks.get_mut(&task_id) {
            task.status = "running".to_string();
            task.message = "Starting FFmpeg...".to_string();
        }
    }

    let mut child = match Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let mut tasks = tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = "failed".to_string();
                task.message = format!("Failed to spawn FFmpeg: {}", e);
            }
            return;
        }
    };

    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);
    let re = Regex::new(r"time=(\d+):(\d+):(\d+\.\d+)").unwrap();

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if let Some(caps) = re.captures(&line) {
                    let h: f64 = caps[1].parse().unwrap_or(0.0);
                    let m: f64 = caps[2].parse().unwrap_or(0.0);
                    let s: f64 = caps[3].parse().unwrap_or(0.0);
                    let current = h * 3600.0 + m * 60.0 + s;
                    if total_duration > 0.0 {
                        let progress = (current / total_duration).min(1.0);
                        let mut tasks = tasks.write().await;
                        if let Some(task) = tasks.get_mut(&task_id) {
                            task.progress = progress;
                            task.message = format!("Processing... {:.0}%", progress * 100.0);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    let status = child.wait().await;

    let mut tasks = tasks.write().await;
    if let Some(task) = tasks.get_mut(&task_id) {
        if task.status == "cancelled" {
            return;
        }

        match status {
            Ok(s) if s.success() => {
                task.status = "completed".to_string();
                task.progress = 1.0;
                if let Some(ref out) = task.output {
                    let size = fs::metadata(format!("/videos/{}", out))
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    task.message = format!("Done! Output: {} ({})", out, fmt_size(size));
                }
            }
            _ => {
                task.status = "failed".to_string();
                task.message = format!("FFmpeg failed");
            }
        }
    }
}

// =========================================================================
// Handlers — Task management
// =========================================================================

async fn list_tasks(State(state): State<AppState>) -> Json<serde_json::Value> {
    cleanup_tasks(&state.tasks).await;
    let tasks = state.tasks.read().await;
    let mut task_list: Vec<&Task> = tasks.values().collect();
    task_list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Json(serde_json::to_value(&task_list).unwrap_or_default())
}

async fn get_task(
    State(state): State<AppState>,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    let tasks = state.tasks.read().await;
    match tasks.get(&task_id) {
        Some(task) => Json(serde_json::to_value(task).unwrap()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Task not found"})),
        )
            .into_response(),
    }
}

async fn cancel_task(
    State(state): State<AppState>,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    let mut tasks = state.tasks.write().await;
    match tasks.get_mut(&task_id) {
        Some(task) => {
            if task.status != "queued" && task.status != "running" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"detail": "Cannot cancel task in this state"})),
                )
                    .into_response();
            }
            task.status = "cancelled".to_string();
            task.message = "Cancelled by user".to_string();
            Json(serde_json::json!({"ok": true})).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"detail": "Task not found"})),
        )
            .into_response(),
    }
}

async fn cleanup_tasks(tasks: &Arc<RwLock<HashMap<String, Task>>>) {
    let now = Utc::now().timestamp();
    let mut tasks = tasks.write().await;
    let to_remove: Vec<String> = tasks
        .iter()
        .filter(|(_, t)| {
            (t.status == "completed" || t.status == "failed" || t.status == "cancelled")
                && chrono::DateTime::parse_from_rfc3339(&t.created_at)
                    .map(|dt| now - dt.timestamp() > 3600)
                    .unwrap_or(false)
        })
        .map(|(k, _)| k.clone())
        .collect();

    for k in to_remove {
        tasks.remove(&k);
    }
}

// =========================================================================
// Main
// =========================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let videos_dir = PathBuf::from(std::env::var("VIDEOS_DIR").unwrap_or_else(|_| "/videos".into()));
    let frontend_dir =
        PathBuf::from(std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "/app/frontend".into()));
    let password = std::env::var("PASSWORD").unwrap_or_default();

    // Secret for HMAC tokens
    let secret_file = PathBuf::from(
        std::env::var("SECRET_FILE").unwrap_or_else(|_| "/data/.secret".into()),
    );
    let secret = if secret_file.is_file() {
        fs::read_to_string(&secret_file)
            .await
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        let s = hex::encode(Uuid::new_v4().to_string())
            + &hex::encode(Uuid::new_v4().to_string());
        let _ = fs::create_dir_all(secret_file.parent().unwrap()).await;
        let _ = fs::write(&secret_file, &s).await;
        s
    };

    if password.is_empty() {
        warn!("⚠️  PASSWORD is not set! Running WITHOUT authentication!");
    }

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let state = AppState {
        videos_dir,
        frontend_dir: frontend_dir.clone(),
        password,
        secret,
        tasks: Arc::new(RwLock::new(HashMap::new())),
    };

    // API routes — catch-all {*path} must be at the end of the route
    let api_routes = Router::new()
        .route("/login", post(login))
        .route("/auth/check", get(auth_check))
        .route("/logout", post(logout))
        .route("/gpu", get(detect_gpu))
        .route("/files", get(list_files))
        .route("/stream/{*path}", get(stream_video))
        .route("/info/{*path}", get(video_info))
        .route("/faststart/{*path}", post(faststart_video))
        .route("/cut", post(cut_video))
        .route("/concat", post(concat_segments))
        .route("/tasks", get(list_tasks))
        .route("/tasks/{task_id}", get(get_task).delete(cancel_task));

    let app = Router::new()
        .nest("/api", api_routes)
        .fallback_service(ServeDir::new(&frontend_dir))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    info!("🚀 NAS Video Editor (Rust) listening on port {}", port);

    axum::serve(listener, app).await.unwrap();
}
