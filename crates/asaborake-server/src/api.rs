//! The HTTP API.
//!
//! Deliberately small: jobs, logos, profiles, and a stream of updates. The web
//! app is the only client, and everything it needs to render is either a list
//! or a subscription.

use std::io::Read as _;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{Job, JobEvent, NewJob};
use crate::worker::Context;

/// Build the router.
pub fn router(context: Context) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/jobs", get(list_jobs).post(submit_job))
        .route("/api/v1/jobs/{id}", get(get_job))
        .route("/api/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/api/v1/jobs/{id}/retry", post(retry_job))
        .route("/api/v1/jobs/{id}/events", get(job_events))
        .route("/api/v1/jobs/{id}/analysis", get(job_analysis))
        .route("/api/v1/logos", get(list_logos))
        .route("/api/v1/logos/scan", post(scan_logo))
        .route(
            "/api/v1/logos/{channel}/{width}/{height}",
            delete(forget_logo),
        )
        .route(
            "/api/v1/logos/no-logo/{channel}",
            post(mark_no_logo).delete(clear_no_logo),
        )
        .route("/api/v1/channels", get(list_channels))
        .route(
            "/api/v1/channels/{id}",
            axum::routing::put(set_channel).delete(forget_channel),
        )
        .route("/api/v1/recordings", get(list_recordings))
        .route("/api/v1/recordings/probe", get(probe_recording))
        .route("/api/v1/frame", get(frame))
        .route("/api/v1/profiles", get(list_profiles))
        .route("/api/v1/events", get(stream_events))
        .with_state(context)
}

/// What went wrong, in a shape the client can render.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<crate::Error> for ApiError {
    fn from(error: crate::Error) -> Self {
        // The detail goes to the log; the client gets something it can show.
        tracing::error!(%error, "request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Liveness, and enough detail to diagnose a misconfigured deployment.
async fn health(State(context): State<Context>) -> Json<Value> {
    let (major, minor) = context.ffmpeg.version();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "ffmpeg": format!("{major}.{minor}"),
        // Which encoders are available decides which profiles can run, and is
        // the first thing to check when a job fails immediately.
        "encoders": {
            "h264_nvenc": context.ffmpeg.has_encoder("h264_nvenc"),
            "hevc_nvenc": context.ffmpeg.has_encoder("hevc_nvenc"),
            "libx264": context.ffmpeg.has_encoder("libx264"),
            "libx265": context.ffmpeg.has_encoder("libx265"),
        },
        "logo_store": context.logos.is_some(),
    }))
}

/// How many jobs to list.
#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

const fn default_limit() -> i64 {
    100
}

async fn list_jobs(
    State(context): State<Context>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<Job>>> {
    Ok(Json(context.store.list(query.limit).await?))
}

async fn submit_job(
    State(context): State<Context>,
    Json(request): Json<NewJob>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if request.input.trim().is_empty() || request.output.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "input and output are both required",
        ));
    }
    if !asaborake_core::profile::builtin().contains_key(&request.profile) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("no profile named '{}'", request.profile),
        ));
    }

    let id = context.store.submit(&request).await?;
    // Wake a worker rather than making it wait for the next poll.
    context.wake.notify_one();

    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn get_job(State(context): State<Context>, Path(id): Path<String>) -> ApiResult<Json<Job>> {
    context
        .store
        .get(&id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("job"))
}

async fn cancel_job(
    State(context): State<Context>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let cancelled = context.store.cancel(&id).await?;
    Ok(Json(json!({ "cancelled": cancelled })))
}

/// Resubmit a job with the same settings.
///
/// A new job rather than a reset of the old one, so the history of what was
/// attempted survives.
async fn retry_job(
    State(context): State<Context>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let job = context
        .store
        .get(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;

    let new_id = context
        .store
        .submit(&NewJob {
            input: job.input,
            output: job.output,
            profile: job.profile,
            title: job.title,
            channel_id: job.channel_id,
            channel_name: job.channel_name,
            priority: job.priority,
        })
        .await?;
    context.wake.notify_one();

    Ok((StatusCode::CREATED, Json(json!({ "id": new_id }))))
}

/// Where to resume a log tail from.
#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: i64,
}

async fn job_events(
    State(context): State<Context>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Json<Vec<JobEvent>>> {
    Ok(Json(context.store.events(&id, query.after).await?))
}

/// The analysis, cut plan and source diagnostics, for the job detail view.
#[derive(Debug, Serialize)]
struct ArtifactsResponse {
    /// The analysis, as stored. Absent until the job has run.
    analysis: Option<Value>,
    /// The cut plan, as stored.
    plan: Option<Value>,
    /// What the source contained and what was wrong with it. Absent when the
    /// source was not a transport stream, or the job predates this being kept.
    diagnostics: Option<Value>,
}

async fn job_analysis(
    State(context): State<Context>,
    Path(id): Path<String>,
) -> ApiResult<Json<ArtifactsResponse>> {
    let artifacts = context
        .store
        .artifacts(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;

    let parse = |text: Option<String>| text.and_then(|text| serde_json::from_str(&text).ok());
    Ok(Json(ArtifactsResponse {
        analysis: parse(artifacts.analysis),
        plan: parse(artifacts.plan),
        diagnostics: parse(artifacts.diagnostics),
    }))
}

/// A logo, as the UI lists it.
#[derive(Debug, Serialize)]
struct LogoSummary {
    name: String,
    channel_id: Option<String>,
    source_width: u32,
    source_height: u32,
    rect: asaborake_analyze::Rect,
    mean_alpha: f32,
    frames_used: u32,
    /// The learned logo as a data URI, so the list renders in one request.
    preview: Option<String>,
}

async fn list_logos(State(context): State<Context>) -> ApiResult<Json<Value>> {
    let Some(store) = context.logos.as_ref() else {
        return Ok(Json(json!({ "logos": [], "channels_without_logos": [] })));
    };

    let logos = store
        .list()
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let summaries: Vec<LogoSummary> = logos
        .into_iter()
        .map(|logo| LogoSummary {
            preview: preview_data_uri(&logo),
            name: logo.name.clone(),
            channel_id: logo.channel_id.clone(),
            source_width: logo.source_width,
            source_height: logo.source_height,
            rect: logo.rect,
            mean_alpha: logo.mean_alpha(),
            frames_used: logo.frames_used,
        })
        .collect();

    Ok(Json(json!({
        "logos": summaries,
        "channels_without_logos": store.channels_without_logos(),
    })))
}

/// Render a logo as a PNG data URI.
///
/// Inlined rather than served from its own endpoint because the list is short
/// and the previews are small; one request beats one per logo.
fn preview_data_uri(logo: &asaborake_analyze::LogoData) -> Option<String> {
    let image = image::RgbaImage::from_raw(logo.rect.width, logo.rect.height, logo.to_rgba())?;
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .ok()
        .map(|()| format!("data:image/png;base64,{}", base64(&png.into_inner())))
}

/// Minimal base64, to avoid a dependency for one use.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let slot = (triple >> (18 - index * 6)) & 0x3F;
                out.push(char::from(ALPHABET[slot as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn forget_logo(
    State(context): State<Context>,
    Path((channel, width, height)): Path<(String, u32, u32)>,
) -> ApiResult<Json<Value>> {
    let Some(store) = context.logos.as_ref() else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no logo store is configured",
        ));
    };
    let removed = store
        .remove(&channel, width, height)
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "removed": removed })))
}

/// Record that a channel carries no logo.
///
/// Not the same as having no logo *yet*: this says one will never be found,
/// so recordings from the channel stop paying three decoding passes to
/// rediscover that, and stop waiting for something that is not coming.
async fn mark_no_logo(
    State(context): State<Context>,
    Path(channel): Path<String>,
) -> ApiResult<Json<Value>> {
    let Some(store) = context.logos.as_ref() else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no logo store is configured",
        ));
    };
    store
        .mark_no_logo(&channel)
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "channel_id": channel, "has_no_logo": true })))
}

/// Look for a logo on this channel again.
async fn clear_no_logo(
    State(context): State<Context>,
    Path(channel): Path<String>,
) -> ApiResult<Json<Value>> {
    let Some(store) = context.logos.as_ref() else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no logo store is configured",
        ));
    };
    let cleared = store
        .clear_no_logo(&channel)
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "channel_id": channel, "cleared": cleared })))
}

/// Per-channel settings, keyed by channel id.
async fn list_channels(State(context): State<Context>) -> Json<Value> {
    Json(json!(
        asaborake_core::ChannelStore::open(&context.config.channels).all()
    ))
}

async fn set_channel(
    State(context): State<Context>,
    Path(id): Path<String>,
    Json(settings): Json<asaborake_core::ChannelSettings>,
) -> ApiResult<Json<Value>> {
    // A profile that does not exist would fail every job on the channel at
    // the moment it starts, which is a slow way to find out about a typo.
    if let Some(profile) = &settings.profile
        && !asaborake_core::profile::builtin().contains_key(profile)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("no profile named '{profile}'"),
        ));
    }

    asaborake_core::ChannelStore::open(&context.config.channels)
        .set(&id, &settings)
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "channel_id": id })))
}

async fn forget_channel(
    State(context): State<Context>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let removed = asaborake_core::ChannelStore::open(&context.config.channels)
        .remove(&id)
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "removed": removed })))
}

async fn list_recordings(State(context): State<Context>) -> Json<Vec<crate::sources::Recording>> {
    Json(crate::sources::list(&context.config.recording_dirs))
}

/// Which recording to describe.
#[derive(Debug, Deserialize)]
struct PathQuery {
    path: String,
}

/// What the logo tool needs to know before it can show a recording.
///
/// The duration bounds the scrubber, and the coded size is what a rectangle
/// drawn on screen has to be converted back into: the scanner works in source
/// pixels, and the browser is showing a scaled, un-squashed picture.
async fn probe_recording(
    State(context): State<Context>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Json<Value>> {
    let Some(path) = crate::sources::resolve(&context.config.recording_dirs, &query.path) else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "that path is not inside a configured recordings directory",
        ));
    };

    let ffmpeg = std::sync::Arc::clone(&context.ffmpeg);
    let probe = tokio::task::spawn_blocking(move || asaborake_media::probe(&ffmpeg, &path))
        .await
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let video = probe.video.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "that recording has no video stream",
        )
    })?;

    Ok(Json(json!({
        "duration_seconds": probe.duration_seconds,
        "width": video.width,
        "height": video.height,
        "fps": video.fps(),
        "interlaced": video.interlaced,
        "services": services_in(&query.path),
    })))
}

/// What the recording calls its own channel.
///
/// Read from the head of the file rather than the whole of it: the service
/// table repeats every couple of seconds, so twenty megabytes is thousands of
/// copies, and someone picking a recording in the logo tool is waiting.
fn services_in(path: &str) -> Vec<asaborake_ts::ServiceInfo> {
    /// Enough of the file to hold many repeats of every table.
    const PREFIX: u64 = 20 * 1024 * 1024;

    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    match asaborake_ts::scan(std::io::BufReader::new(file).take(PREFIX), PREFIX) {
        Ok(info) => info.services,
        Err(error) => {
            tracing::debug!(%error, "could not read the services from this recording");
            Vec::new()
        }
    }
}

/// Which frame of which recording to show.
#[derive(Debug, Deserialize)]
struct FrameQuery {
    /// Absolute path, as `/api/v1/recordings` gave it.
    path: String,
    /// Position in the recording, in seconds.
    #[serde(default)]
    at: f64,
    /// Width to render at.
    #[serde(default = "default_frame_width")]
    width: u32,
}

const fn default_frame_width() -> u32 {
    960
}

/// Serve one frame of a recording as a PNG.
///
/// This is what makes the logo tool possible: without seeing the picture there
/// is no way to draw a rectangle over the logo, which is the one thing that
/// makes detection work reliably on real broadcast.
async fn frame(State(context): State<Context>, Query(query): Query<FrameQuery>) -> Response {
    let Some(path) = crate::sources::resolve(&context.config.recording_dirs, &query.path) else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "that path is not inside a configured recordings directory",
        )
        .into_response();
    };

    let ffmpeg = std::sync::Arc::clone(&context.ffmpeg);
    let at = query.at.max(0.0);
    let width = query.width;
    // Decoding is blocking and can take a moment on a large recording; it must
    // not be done on a thread that is also serving the event stream.
    let rendered =
        tokio::task::spawn_blocking(move || asaborake_media::still_png(&ffmpeg, &path, at, width))
            .await;

    match rendered {
        Ok(Ok(png)) if !png.is_empty() => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "image/png"),
                // A given frame of a given recording never changes, and
                // scrubbing revisits the same positions constantly.
                (axum::http::header::CACHE_CONTROL, "private, max-age=3600"),
            ],
            png,
        )
            .into_response(),
        Ok(Ok(_)) => ApiError::new(
            StatusCode::NOT_FOUND,
            "there is no frame at that position; the recording may be shorter than it claims",
        )
        .into_response(),
        Ok(Err(error)) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
        Err(error) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
    }
}

/// A request to learn a logo from a rectangle someone drew.
#[derive(Debug, Deserialize)]
struct ScanRequest {
    /// Recording to learn from.
    path: String,
    /// Where the logo is, in source pixels.
    rect: asaborake_analyze::Rect,
    /// Channel this logo belongs to, which is what jobs look it up by.
    #[serde(default)]
    channel_id: Option<String>,
    /// Human-readable name.
    #[serde(default)]
    name: Option<String>,
}

/// Learn a logo from a rectangle and add it to the store.
///
/// Runs synchronously: it is one decoding pass over a recording, which takes
/// tens of seconds, and the operator who drew the rectangle is waiting to see
/// whether it worked. Making it a queued job would hide the answer behind a
/// second screen.
async fn scan_logo(
    State(context): State<Context>,
    Json(request): Json<ScanRequest>,
) -> ApiResult<Json<Value>> {
    let Some(store) = context.logos.as_ref() else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "no logo store is configured, so there is nowhere to keep the result",
        ));
    };
    let Some(path) = crate::sources::resolve(&context.config.recording_dirs, &request.path) else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "that path is not inside a configured recordings directory",
        ));
    };
    if !request.rect.is_valid() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the rectangle has no area",
        ));
    }

    let ffmpeg = std::sync::Arc::clone(&context.ffmpeg);
    let store = std::sync::Arc::clone(store);
    let rect = request.rect;
    let options = asaborake_analyze::AnalysisOptions {
        logo_name: request
            .name
            .clone()
            .or_else(|| request.channel_id.clone())
            .unwrap_or_else(|| "unnamed".to_owned()),
        channel_id: request.channel_id.clone(),
        ..asaborake_analyze::AnalysisOptions::default()
    };

    let learned = tokio::task::spawn_blocking(move || {
        let logo = asaborake_analyze::learn(&ffmpeg, &path, rect, &options, &mut |_| {})?;
        if let Some(logo) = logo {
            store.save(&logo).ok();
            return Ok::<_, asaborake_analyze::Error>(Ok(logo));
        }
        // Nothing usable. Measure the same rectangle again without the
        // plausibility bar, so the answer can say *why* rather than just no.
        let report = asaborake_analyze::scan_rect(&ffmpeg, &path, rect, &options, &mut |_| {})?;
        Ok(Err(report))
    })
    .await
    .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let logo = match learned {
        Ok(logo) => logo,
        Err(report) => {
            return Ok(Json(json!({
                "learned": false,
                "reason": report.explain(),
                "frames_used": report.frames_used,
                "background_spread": report.background_spread,
                "mean_alpha": report.mean_alpha,
                "strong_pixels": report.strong_pixels,
                // The rejected fit, so an operator can see whether the box was
                // aimed at the right thing. A recognisable logo that failed the
                // bar and a rectangle full of noise look nothing alike, and no
                // number conveys the difference as fast as the picture does.
                "preview": report.logo.as_ref().and_then(preview_data_uri),
            })));
        }
    };

    Ok(Json(json!({
        "learned": true,
        "name": logo.name,
        "channel_id": logo.channel_id,
        "source_width": logo.source_width,
        "source_height": logo.source_height,
        "rect": logo.rect,
        "mean_alpha": logo.mean_alpha(),
        "frames_used": logo.frames_used,
        "preview": preview_data_uri(&logo),
    })))
}

async fn list_profiles(State(context): State<Context>) -> Json<Value> {
    let profiles: Vec<Value> = asaborake_core::profile::builtin()
        .into_iter()
        .map(|(name, profile)| {
            json!({
                "name": name,
                "description": profile.description,
                "container": profile.container,
                "encoder": profile.video.encoder,
                // A profile the build cannot run is shown but not offered.
                "available": profile.is_supported_by(&context.ffmpeg),
            })
        })
        .collect();
    Json(json!(profiles))
}

/// Live progress and log lines.
async fn stream_events(
    State(context): State<Context>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut receiver = context.events.subscribe();

    let stream = async_stream::stream! {
        loop {
            match receiver.recv().await {
                Ok(update) => {
                    if let Ok(json) = serde_json::to_string(&update) {
                        yield Ok(Event::default().data(json));
                    }
                }
                // A client that fell behind has missed some snapshots. The
                // next one supersedes them, so it simply carries on.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "client fell behind the update stream");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Without keep-alives an idle stream is indistinguishable from a dead one
    // to any proxy in between.
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_bytes_above_the_ascii_range() {
        assert_eq!(base64(&[0xFF, 0xFE, 0xFD]), "//79");
        assert_eq!(base64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn an_api_error_renders_as_json() {
        let response = ApiError::not_found("job").into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_default_listing_limit_is_reasonable() {
        assert_eq!(default_limit(), 100);
    }
}
