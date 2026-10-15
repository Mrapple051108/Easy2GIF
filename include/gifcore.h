/*
 * gifcore.h — C ABI of the Rust video-to-GIF kernel.
 *
 * The C++ / QML layer only ever talks to the kernel through this header. It
 * never links Rust types, and it never touches video data itself: it forwards
 * user intent in, and presents the state and pixels the kernel hands back.
 *
 * Return convention
 * -----------------
 * Status-returning calls answer GIF_OK (0) on success and a NEGATIVE GIF_ERR_*
 * code on failure. Boolean queries (gif_has_media, gif_is_exporting) answer
 * 1/0. On failure gif_last_error() holds a UTF-8, human-readable reason that
 * stays valid until the next call on the same thread.
 *
 * Concurrency
 * -----------
 * One export/estimate per handle; overlapping jobs return GIF_ERR_BUSY.
 * gif_cancel may run concurrently with a job. Destroy only after all calls finish.
 * Borrowed buffers require external serialization against load/close/preview rebuild.
 *
 * Buffer lifetime
 * ---------------
 * gif_poster_rgb() and gif_thumb_rgb() hand out a borrowed view into storage
 * owned by the handle. It stays valid until the next call that rebuilds it, so
 * the caller must copy before asking again.
 */

#ifndef GIFCORE_H
#define GIFCORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define GIFCORE_ABI_VERSION 7u

typedef int32_t gif_status;

#define GIF_OK 0
#define GIF_ERR_INVALID (-1)
#define GIF_ERR_IO (-2)
#define GIF_ERR_MEDIA (-3)
#define GIF_ERR_TOOL (-4)
#define GIF_ERR_ENCODE (-5)
#define GIF_ERR_UNKNOWN (-6)
#define GIF_ERR_CANCELLED (-8)
#define GIF_ERR_BUSY (-9)
/*
 * The GIF could not be written where it was asked to go: the folder cannot be
 * created or is not writable, the target is read-only, or it is held open by
 * another program. Checked up front by gif_check_output().
 */
#define GIF_ERR_OUTPUT (-7)

/** Opaque kernel handle. */
typedef struct GifCore GifCore;

/** A borrowed byte buffer. */
typedef struct {
    const uint8_t *data;
    size_t len;
} GifCoreBuffer;

/** One timeline thumbnail, tightly packed RGB24. */
typedef struct {
    const uint8_t *data;
    size_t len;
    uint32_t width;
    uint32_t height;
    double time;
} GifCoreThumb;

/** A filmstrip of `count` thumbnails. */
typedef struct {
    const GifCoreThumb *thumbs;
    uint32_t count;
    uint64_t source_frames;
} GifCoreFilmstrip;

/** Statistics of the most recent export. */
typedef struct {
    uint64_t frames;
    uint64_t bytes;
    uint32_t width;
    uint32_t height;
    double fps;
    double duration; /* actual GIF playback seconds, including speed and delay rounding */
    uint32_t palette_colors;
    uint64_t elapsed_ms;
} GifCoreExportResult;

/*
 * Every knob of an export. Zero means "use the documented default", so a caller
 * can zero-initialise the struct and set only what it cares about.
 * Exception in ABI 7: dither_strength=0 explicitly disables dithering; set 100 for default.
 *
 * Added in ABI 5 together with gif_export_ex() and gif_estimate_size(). The
 * older fixed-argument gif_export() is kept and forwards to gif_export_ex().
 *
 * ABI 6 added `segments` to GifCoreEstimate (the estimate now samples several
 * short segments spread over the selection instead of one window).
 */
typedef struct {
    double fps;               /* 0 => 12 */
    uint32_t max_width;       /* 0 => 480 */
    uint32_t colors;          /* 0 => 256, clamped to 2..=256 */
    const char *dither;       /* "none" | "ordered" | "floyd"; NULL => floyd */
    double speed;             /* 0 => 1.0 */
    int loop_forever;         /* non-zero repeats the GIF forever */
    uint32_t threads;         /* 0 => gif_default_threads() */
    /* --- 高级设置 --- */
    uint32_t quality;         /* 编码质量 1..=10; 0 => 5, the historical output */
    uint32_t lossy;           /* 有损压缩 0..=100; 0 => off */
    uint32_t dither_strength; /* ABI 7: 0..=100; 0 disables, set 100 for default */
    int dedupe;               /* 复用重复帧: fold equal frames together */
    int optimize;             /* 帧间透明优化: leave unchanged pixels clear */
    uint32_t tolerance;       /* per-channel slack 0..=64 for `optimize` */
} GifCoreExportOptions;

/*
 * Projected size of an export: the real pipeline is run over a few short
 * segments spread over the selection and the result extrapolated. `exact` is
 * non-zero when the whole selection was measured, in which case `bytes` is the
 * true size. `segments` reports how many separate moments `sampled` frames came
 * from, so the caller can say so in the UI.
 */
typedef struct {
    uint64_t bytes;
    uint64_t frames;
    uint64_t sampled;
    uint64_t segments;
    uint32_t width;
    uint32_t height;
    int exact;
} GifCoreEstimate;

uint32_t gif_abi_version(void);
const char *gif_version(void);

/** Logical CPUs available to this process, read at call time. */
uint32_t gif_cpu_count(void);
/** Worker count gif_export() uses when passed `threads = 0`: half the CPUs. */
uint32_t gif_default_threads(void);
/** Clamp a requested worker count the way gif_export() does. */
uint32_t gif_resolve_threads(uint32_t requested);

/**
 * Integrity level of this process: 0 unknown, 1 low, 2 medium, 3 high,
 * 4 system.
 *
 * Windows refuses writes from a Low-integrity process to any object that is not
 * itself Low-labelled, so a sandboxed launcher makes *every* ordinary folder
 * answer "拒绝访问" however permissive its ACL is. The front end uses this to
 * say so, instead of blaming the folder the user picked.
 */
int gif_integrity_level(void);

GifCore *gif_create(void);
void gif_destroy(GifCore *h);

const char *gif_last_error(GifCore *h);

/** Override tool discovery. Both are optional; discovery is automatic. */
gif_status gif_set_ffmpeg_path(GifCore *h, const char *path);
gif_status gif_set_ffprobe_path(GifCore *h, const char *path);
/** Verify both media tools actually run. */
gif_status gif_check_tools(GifCore *h);

gif_status gif_load(GifCore *h, const char *path);
gif_status gif_close(GifCore *h);
int gif_has_media(GifCore *h);

uint32_t gif_width(GifCore *h);
uint32_t gif_height(GifCore *h);
double gif_fps(GifCore *h);
double gif_fps_display(GifCore *h);
double gif_duration(GifCore *h);
uint64_t gif_frame_count(GifCore *h);
int gif_has_audio(GifCore *h);
uint64_t gif_total_frames(GifCore *h);
const char *gif_video_codec(GifCore *h);

/** Selection is expressed in seconds and clamped to the clip. */
gif_status gif_set_selection(GifCore *h, double start, double end);
double gif_selection_start(GifCore *h);
double gif_selection_end(GifCore *h);
double gif_selection_duration(GifCore *h);
uint64_t gif_selection_start_frame(GifCore *h);
uint64_t gif_selection_end_frame(GifCore *h);
uint64_t gif_estimate_frames(GifCore *h, double fps);
uint64_t gif_estimate_frames_ex(GifCore *h, double fps, double speed);

/** Re-decode the poster frame at the current start time. */
gif_status gif_update_poster(GifCore *h);
GifCoreBuffer gif_poster_rgb(GifCore *h);
uint32_t gif_poster_width(GifCore *h);
uint32_t gif_poster_height(GifCore *h);

/** Rebuild the timeline filmstrip at `thumb_w` pixels wide per thumbnail. */
gif_status gif_build_filmstrip(GifCore *h, uint32_t count, uint32_t thumb_w);
GifCoreFilmstrip gif_filmstrip(GifCore *h);
GifCoreBuffer gif_thumb_rgb(GifCore *h, uint32_t index);

/**
 * Encode the selected range to `out_path`. This blocks, so the UI runs it on a
 * worker thread and polls gif_progress(). `dither` is "none", "ordered" or
 * "floyd"; NULL means "floyd".
 *
 * `threads` is the worker count for the palette search and the per-frame
 * mapping/LZW pass. 0 means "use gif_default_threads()", and anything above
 * gif_cpu_count() is clamped down to it. The output is byte-for-byte identical
 * whatever this is, so it is purely a speed control.
 */
/*
 * Check that an export could actually be written to out_path, without running
 * one. Returns GIF_OK when the location is usable, otherwise GIF_ERR_OUTPUT
 * with the reason in gif_last_error(). Needs no loaded media.
 */
gif_status gif_check_output(GifCore *h, const char *out_path);

gif_status gif_export(GifCore *h, const char *out_path, double fps,
                      uint32_t max_width, uint32_t colors, const char *dither,
                      double speed, int loop_forever, uint32_t threads);

/*
 * gif_export() with the advanced settings. `opts` may be NULL, which means
 * "everything at its default"; it is read once, up front, and never retained.
 */
gif_status gif_export_ex(GifCore *h, const char *out_path,
                         const GifCoreExportOptions *opts);

/*
 * Fill `out` with the projected size of the export `opts` describes, without
 * writing a file. This decodes video and blocks for a fraction of a second, so
 * the front end calls it from a worker thread and discards stale answers.
 */
gif_status gif_estimate_size(GifCore *h, const GifCoreExportOptions *opts,
                             GifCoreEstimate *out);

gif_status gif_cancel(GifCore *h);
int gif_is_exporting(GifCore *h);
double gif_progress(GifCore *h);
uint32_t gif_progress_phase(GifCore *h);
const char *gif_progress_phase_name(GifCore *h);

GifCoreExportResult gif_last_result(GifCore *h);
const char *gif_last_result_summary(GifCore *h);
const char *gif_last_warning(GifCore *h);
gif_status gif_reset_progress(GifCore *h);

#ifdef __cplusplus
}
#endif

#endif /* GIFCORE_H */
