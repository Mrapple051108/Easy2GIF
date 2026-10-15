#include "GifBridge.h"

#include "gifcore.h"

#include <QDir>
#include <QFileInfo>
#include <QRegularExpression>
#include <QUrl>

#include <algorithm>
#include <cstring>

namespace {

/// Map the combo-box index onto the kernel's dither names.
const char *ditherName(int index)
{
    switch (index) {
    case 0:
        return "none";
    case 1:
        return "ordered";
    default:
        return "floyd";
    }
}

/// Copy a tightly packed RGB24 buffer into a QImage.
///
/// The kernel hands out a borrowed view it may rewrite on the next call, so the
/// bytes are copied row by row rather than aliased.
QImage imageFromRgb(const uint8_t *data, int width, int height)
{
    if (!data || width <= 0 || height <= 0) {
        return QImage();
    }
    QImage image(width, height, QImage::Format_RGB888);
    if (image.isNull()) {
        return QImage();
    }
    const int rowBytes = width * 3;
    for (int y = 0; y < height; ++y) {
        std::memcpy(image.scanLine(y), data + static_cast<size_t>(y) * rowBytes,
                    static_cast<size_t>(rowBytes));
    }
    return image;
}

QString decodeError(const char *raw)
{
    if (!raw) {
        return QString();
    }
    return QString::fromUtf8(raw);
}

/// Sizes the way a person reads them: "707.6 KB", "1.4 MB". The kernel formats
/// finished exports itself; this is for projections, which never reach it.
QString formatBytes(unsigned long long bytes)
{
    const double value = static_cast<double>(bytes);
    if (bytes < 1024ULL) {
        return QStringLiteral("%1 B").arg(bytes);
    }
    if (bytes < 1024ULL * 1024ULL) {
        return QStringLiteral("%1 KB").arg(value / 1024.0, 0, 'f', 1);
    }
    return QStringLiteral("%1 MB").arg(value / (1024.0 * 1024.0), 0, 'f', 2);
}

/// GIF is the only thing we write, so the suffix is not a choice the user has
/// to get right.
QString ensureGifSuffix(const QString &name)
{
    return name.endsWith(QLatin1String(".gif"), Qt::CaseInsensitive)
               ? name
               : name + QStringLiteral(".gif");
}

/// The extra explanation to append when a write was refused *only* because this
/// process is running at Low integrity.
///
/// Windows takes a process's integrity level from the label on its *image file*,
/// and its mandatory policy then refuses every write to an ordinary folder —
/// before that folder's ACL is consulted at all. The raw error, "拒绝访问。
/// (os error 5)", is word-for-word the same as a folder that really is read-only,
/// so without this the user goes looking for a permission problem that does not
/// exist. The build tree and the programs in it are deliberately unlabelled
/// (Medium), and a Low process cannot raise its own label — the privilege it
/// would need is exactly what the sandbox withholds — so seeing this message
/// means the program was started from a confined context: start it from Explorer
/// or the Start menu instead. Empty when the process is not Low, so the caller's
/// ordinary wording stands on its own.
QString sandboxRefusal(const QString &folder)
{
    Q_UNUSED(folder);
    if (gif_integrity_level() != 1) {
        return QString();
    }
    // Kept to two lines on purpose: this lands in the export card, and a long
    // explanation pushes the button off the bottom of the window. The full
    // wording lives in the header chip's tooltip.
    return QStringLiteral(
        "\n本程序以低完整性运行（沙箱），系统不允许低完整性进程写入普通文件夹，"
        "和这个文件夹的权限设置无关。\n"
        "请从资源管理器或开始菜单直接启动本程序 —— 不要从沙箱/受限的终端里启动它。");
}

} // namespace

// ---------------------------------------------------------------------------
// GifImageProvider
// ---------------------------------------------------------------------------

GifImageProvider::GifImageProvider(GifBridge *bridge)
    : QQuickImageProvider(QQuickImageProvider::Image), m_bridge(bridge)
{
}

QImage GifImageProvider::requestImage(const QString &id, QSize *size,
                                     const QSize &requestedSize)
{
    Q_UNUSED(requestedSize);

    // QML appends "?r=<n>" to defeat the provider cache, so drop the query.
    QString key = id;
    const int query = key.indexOf(QLatin1Char('?'));
    if (query >= 0) {
        key = key.left(query);
    }

    QImage image;
    if (key == QLatin1String("poster")) {
        image = m_bridge->posterImage();
    } else if (key == QLatin1String("strip")) {
        image = m_bridge->stripImage();
    } else if (key.startsWith(QLatin1String("thumb/"))) {
        image = m_bridge->thumbImage(key.mid(6).toInt());
    }

    if (size) {
        *size = image.size();
    }
    return image;
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

GifBridge::GifBridge(QObject *parent)
    : QObject(parent)
{
    m_core = gif_create();
    if (m_core) {
        m_coreVersion = QString::fromUtf8(gif_version());
    }

    // The poster is decoded by spawning ffmpeg, so coalesce rapid drags into a
    // single decode instead of queuing one per mouse move.
    m_posterTimer.setSingleShot(true);
    m_posterTimer.setInterval(220);
    connect(&m_posterTimer, &QTimer::timeout, this, &GifBridge::onPosterDebounce);

    m_pollTimer.setInterval(80);
    connect(&m_pollTimer, &QTimer::timeout, this, &GifBridge::onPollTick);

    // The size estimate decodes real video, so a slider being dragged must not
    // start one per pixel: wait for the user to stop, then measure once.
    m_estimateTimer.setSingleShot(true);
    m_estimateTimer.setInterval(350);
    connect(&m_estimateTimer, &QTimer::timeout, this, &GifBridge::onEstimateDebounce);

    // Everything that can move the projected size asks for a new estimate. This
    // only restarts a timer, so the setters themselves stay non-blocking.
    connect(this, &GifBridge::settingsChanged, this, &GifBridge::scheduleEstimate);
    connect(this, &GifBridge::selectionChanged, this, &GifBridge::scheduleEstimate);
    connect(this, &GifBridge::mediaChanged, this, &GifBridge::scheduleEstimate);
}

GifBridge::~GifBridge()
{
    m_estimateTimer.stop();
    // A queued export must not start during teardown.
    m_startExportWhenIdle = nullptr;
    if (m_exporting) {
        if (m_core) {
            gif_cancel(m_core);
        }
        if (m_worker.joinable()) {
            m_worker.join();
        }
    }
    // An estimate holds the kernel handle too, so it must be finished with
    // before the handle goes away.
    if (m_estimateWorker.joinable()) {
        m_estimateWorker.join();
    }
    if (m_core) {
        gif_destroy(m_core);
        m_core = nullptr;
    }
}

// ---------------------------------------------------------------------------
// Tooling
// ---------------------------------------------------------------------------

QString GifBridge::takeError() const
{
    if (!m_core) {
        return QStringLiteral("kernel handle is not available");
    }
    const QString message = decodeError(gif_last_error(m_core));
    return message.isEmpty() ? QStringLiteral("the kernel reported a failure")
                             : message;
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

bool GifBridge::sandboxed() const
{
    // 1 is `Integrity::Low` on the C ABI. The kernel reads the integrity level
    // out of this process's token; a restricted launcher is what produces it.
    return gif_integrity_level() == 1;
}

QString GifBridge::sandboxText() const
{
    return sandboxed() ? QStringLiteral("沙箱运行：导出到普通文件夹会被系统拒绝")
                       : QStringLiteral("普通权限");
}

bool GifBridge::checkTools()
{
    if (!m_core) {
        m_toolsReady = false;
        m_toolsStatus = QStringLiteral("Rust 内核未初始化");
        emit toolsChanged();
        return false;
    }

    const gif_status status = gif_check_tools(m_core);
    m_toolsReady = (status == GIF_OK);
    m_toolsStatus = m_toolsReady
                        ? QStringLiteral("视频引擎就绪（内置，无需安装）")
                        : QStringLiteral("视频引擎不可用：%1").arg(takeError());
    // The kernel exists now, so the save-location check can actually run.
    refreshOutputCheck();
    emit toolsChanged();
    return m_toolsReady;
}

QString GifBridge::defaultOutputPath() const
{
    return QDir(defaultOutputDir()).filePath(defaultOutputName());
}

QString GifBridge::defaultOutputDir() const
{
    if (!m_videoPath.isEmpty()) {
        const QFileInfo info(m_videoPath);
        return QDir::toNativeSeparators(info.absolutePath());
    }
    return QDir::toNativeSeparators(QDir::homePath());
}

QString GifBridge::defaultOutputName() const
{
    if (!m_videoPath.isEmpty()) {
        return QFileInfo(m_videoPath).completeBaseName() + QStringLiteral(".gif");
    }
    return QStringLiteral("output.gif");
}

QString GifBridge::outputProblem(const QString &folder, const QString &fileName) const
{
    if (!m_core) {
        return QStringLiteral("内核尚未就绪");
    }
    const QString folderPath = QDir::toNativeSeparators(folder.trimmed());
    const QString name = fileName.trimmed();
    if (folderPath.isEmpty()) {
        return QStringLiteral("请先选择要保存到的文件夹");
    }
    if (name.isEmpty()) {
        return QStringLiteral("请先填写文件名");
    }
    if (name.contains(QLatin1Char('/')) || name.contains(QLatin1Char('\\'))) {
        return QStringLiteral("文件名里不能带路径分隔符，文件夹请写在上面那一栏");
    }

    const QByteArray target =
        QDir(folderPath).filePath(ensureGifSuffix(name)).toUtf8();
    if (gif_check_output(m_core, target.constData()) == GIF_OK) {
        return QString();
    }

    // The kernel's text gives the reason, but the OS part alone ("拒绝访问。
    // (os error 5)") is exactly what left the user asking why. Lead with the
    // plain-language cause and keep the detail underneath.
    const QString detail = decodeError(gif_last_error(m_core));
    const QString sandbox = sandboxRefusal(folderPath);
    if (!sandbox.isEmpty()) {
        return QStringLiteral("这个位置现在写不进去（拒绝访问）。%1\n%2")
            .arg(sandbox, detail);
    }
    return QStringLiteral("这个位置现在写不进去：文件可能正被其他程序打开，"
                          "或该文件夹没有写入权限。\n%1")
        .arg(detail);
}

QString GifBridge::outputTargetText() const
{
    if (m_outputDir.isEmpty() || m_outputName.isEmpty()) {
        return QString();
    }
    // QDir::filePath always joins with '/', which reads oddly in a Windows app.
    return QDir::toNativeSeparators(
        QDir(m_outputDir).filePath(ensureGifSuffix(m_outputName)));
}

void GifBridge::setOutputDir(const QString &dir)
{
    const QString normalised = QDir::toNativeSeparators(dir.trimmed());
    if (m_outputDir == normalised) {
        return;
    }
    m_outputDir = normalised;
    refreshOutputCheck();
    emit outputChanged();
}

void GifBridge::setOutputName(const QString &name)
{
    const QString trimmed = name.trimmed();
    if (m_outputName == trimmed) {
        return;
    }
    m_outputName = trimmed;
    refreshOutputCheck();
    emit outputChanged();
}

void GifBridge::refreshOutputCheck()
{
    m_outputProblem = outputProblem(m_outputDir, m_outputName);
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

QString GifBridge::toLocalPath(const QString &urlOrPath) const
{
    QString path = urlOrPath.trimmed();
    if (path.startsWith(QLatin1String("file:"), Qt::CaseInsensitive)) {
        path = QUrl(path).toLocalFile();
    }
    return QDir::toNativeSeparators(path);
}

bool GifBridge::openVideo(const QString &pathOrUrl)
{
    if (!m_core) {
        return false;
    }

    const QString path = toLocalPath(pathOrUrl);

    if (path.trimmed().isEmpty()) {
        m_lastError = QStringLiteral("没有选择文件");
        emit resultChanged();
        return false;
    }

    m_posterTimer.stop();
    m_filmstripCount = 0;

    const gif_status status = gif_load(m_core, path.toUtf8().constData());
    if (status != GIF_OK) {
        fail(QStringLiteral("打开视频失败"));
        emit filmstripChanged();
        return false;
    }

    m_videoPath = path;
    refreshMediaFromKernel();

    // Offer the source's own folder and name first: that is where a user
    // almost always wants the GIF, and it is the one place we know exists.
    setOutputDir(defaultOutputDir());
    setOutputName(defaultOutputName());

    m_start = 0.0;
    m_end = m_duration;
    pushSelectionToKernel();

    m_lastError.clear();
    emit mediaChanged();
    emit selectionChanged();
    emit filmstripChanged();
    emit resultChanged();

    refreshPoster();
    buildFilmstrip(12, 160);
    return true;
}

void GifBridge::closeVideo()
{
    if (!m_core) {
        return;
    }
    m_posterTimer.stop();
    gif_close(m_core);
    m_mediaLoaded = false;
    m_videoPath.clear();
    m_videoCodec.clear();
    m_hasAudio = false;
    m_width = m_height = 0;
    m_fps = m_duration = 0.0;
    m_frameCount = 0;
    m_start = m_end = 0.0;
    m_filmstripCount = 0;
    m_hasResult = false;
    m_lastSummary.clear();
    m_lastWarning.clear();
    setOutputDir(QString());
    setOutputName(QString());
    emit mediaChanged();
    emit selectionChanged();
    emit filmstripChanged();
    emit posterChanged();
    emit resultChanged();
}

void GifBridge::refreshMediaFromKernel()
{
    if (!m_core) {
        return;
    }
    m_mediaLoaded = gif_has_media(m_core) != 0;
    m_width = static_cast<int>(gif_width(m_core));
    m_height = static_cast<int>(gif_height(m_core));
    m_fps = gif_fps(m_core);
    m_duration = gif_duration(m_core);
    m_frameCount = gif_frame_count(m_core);
    m_hasAudio = gif_has_audio(m_core) != 0;
    m_videoCodec = QString::fromUtf8(gif_video_codec(m_core));
}

void GifBridge::fail(const QString &context)
{
    m_lastError = QStringLiteral("%1：%2").arg(context, takeError());
    emit resultChanged();
    emit errorOccurred(m_lastError);
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

void GifBridge::pushSelectionToKernel()
{
    if (!m_core) {
        return;
    }
    const gif_status status = gif_set_selection(m_core, m_start, m_end);
    if (status != GIF_OK) {
        fail(QStringLiteral("设置选区失败"));
        return;
    }
    // The kernel clamps and orders the pair, so adopt what it settled on.
    m_start = gif_selection_start(m_core);
    m_end = gif_selection_end(m_core);
    emit selectionChanged();
}

void GifBridge::setStartTime(double seconds)
{
    if (!m_mediaLoaded) {
        return;
    }
    m_start = seconds;
    pushSelectionToKernel();
    m_posterTimer.start();
}

void GifBridge::setEndTime(double seconds)
{
    if (!m_mediaLoaded) {
        return;
    }
    m_end = seconds;
    pushSelectionToKernel();
}

void GifBridge::setStartFraction(double fraction)
{
    if (!m_mediaLoaded || m_duration <= 0.0) {
        return;
    }
    setStartTime(std::clamp(fraction, 0.0, 1.0) * m_duration);
}

void GifBridge::setEndFraction(double fraction)
{
    if (!m_mediaLoaded || m_duration <= 0.0) {
        return;
    }
    setEndTime(std::clamp(fraction, 0.0, 1.0) * m_duration);
}

void GifBridge::nudgeStart(double deltaSeconds)
{
    setStartTime(m_start + deltaSeconds);
}

void GifBridge::nudgeEnd(double deltaSeconds)
{
    setEndTime(m_end + deltaSeconds);
}

void GifBridge::selectWholeClip()
{
    if (!m_mediaLoaded) {
        return;
    }
    m_start = 0.0;
    m_end = m_duration;
    pushSelectionToKernel();
    m_posterTimer.start();
}

void GifBridge::clearSelection()
{
    if (!m_mediaLoaded) {
        return;
    }
    // Collapse to the single frame under the start handle.
    const double step = m_fps > 0.0 ? 1.0 / m_fps : 0.04;
    m_end = std::min(m_duration, m_start + step);
    pushSelectionToKernel();
}

double GifBridge::selectionDuration() const
{
    return std::max(0.0, m_end - m_start);
}

double GifBridge::startFraction() const
{
    return m_duration > 0.0 ? std::clamp(m_start / m_duration, 0.0, 1.0) : 0.0;
}

double GifBridge::endFraction() const
{
    return m_duration > 0.0 ? std::clamp(m_end / m_duration, 0.0, 1.0) : 0.0;
}

QString GifBridge::startFrameText() const
{
    if (!m_core || !m_mediaLoaded) {
        return QStringLiteral("0");
    }
    return QString::number(static_cast<qulonglong>(gif_selection_start_frame(m_core)));
}

QString GifBridge::endFrameText() const
{
    if (!m_core || !m_mediaLoaded) {
        return QStringLiteral("0");
    }
    return QString::number(static_cast<qulonglong>(gif_selection_end_frame(m_core)));
}

QString GifBridge::selectionFramesText() const
{
    if (!m_core || !m_mediaLoaded) {
        return QStringLiteral("0");
    }
    return QString::number(
        static_cast<qulonglong>(gif_estimate_frames_ex(m_core, m_outFps, m_speed)));
}

QString GifBridge::estimatedFramesText() const
{
    return selectionFramesText();
}

// ---------------------------------------------------------------------------
// Time formatting
// ---------------------------------------------------------------------------

QString GifBridge::formatTime(double seconds) const
{
    if (!(seconds >= 0.0)) {
        seconds = 0.0;
    }
    const qint64 totalMs = static_cast<qint64>(qRound(seconds * 1000.0));
    const qint64 ms = totalMs % 1000;
    const qint64 totalSeconds = totalMs / 1000;
    const qint64 s = totalSeconds % 60;
    const qint64 m = (totalSeconds / 60) % 60;
    const qint64 h = totalSeconds / 3600;

    if (h > 0) {
        return QStringLiteral("%1:%2:%3.%4")
            .arg(h)
            .arg(m, 2, 10, QLatin1Char('0'))
            .arg(s, 2, 10, QLatin1Char('0'))
            .arg(ms, 3, 10, QLatin1Char('0'));
    }
    return QStringLiteral("%1:%2.%3")
        .arg(m, 2, 10, QLatin1Char('0'))
        .arg(s, 2, 10, QLatin1Char('0'))
        .arg(ms, 3, 10, QLatin1Char('0'));
}

double GifBridge::parseTime(const QString &text) const
{
    const QString trimmed = text.trimmed();
    if (trimmed.isEmpty()) {
        return -1.0;
    }

    // "150f" / "150F" addresses a frame index directly.
    if (trimmed.endsWith(QLatin1Char('f'), Qt::CaseInsensitive)) {
        bool ok = false;
        const double frame = trimmed.left(trimmed.size() - 1).trimmed().toDouble(&ok);
        if (!ok || frame < 0.0 || m_fps <= 0.0) {
            return -1.0;
        }
        return frame / m_fps;
    }

    if (trimmed.contains(QLatin1Char(':'))) {
        const QStringList parts = trimmed.split(QLatin1Char(':'));
        if (parts.size() < 2 || parts.size() > 3) {
            return -1.0;
        }
        double scale = 1.0;
        double total = 0.0;
        for (int i = parts.size() - 1; i >= 0; --i) {
            bool ok = false;
            const double value = parts.at(i).trimmed().toDouble(&ok);
            if (!ok || value < 0.0) {
                return -1.0;
            }
            total += value * scale;
            scale *= 60.0;
        }
        return total;
    }

    bool ok = false;
    const double value = trimmed.toDouble(&ok);
    if (!ok || value < 0.0) {
        return -1.0;
    }
    return value;
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

void GifBridge::refreshPoster()
{
    if (!m_core || !m_mediaLoaded) {
        return;
    }
    const gif_status status = gif_update_poster(m_core);
    if (status != GIF_OK) {
        return; // A missing preview is not worth interrupting the user for.
    }
    ++m_posterRevision;
    emit posterChanged();
}

void GifBridge::onPosterDebounce()
{
    refreshPoster();
}

void GifBridge::buildFilmstrip(int count, int thumbWidth)
{
    if (!m_core || !m_mediaLoaded) {
        return;
    }
    const int clampedCount = std::clamp(count, 2, 40);
    const int clampedWidth = std::clamp(thumbWidth, 32, 480);
    const gif_status status = gif_build_filmstrip(
        m_core, static_cast<uint32_t>(clampedCount),
        static_cast<uint32_t>(clampedWidth));
    if (status != GIF_OK) {
        m_filmstripCount = 0;
        emit filmstripChanged();
        return;
    }
    m_filmstripCount = static_cast<int>(gif_filmstrip(m_core).count);
    ++m_filmstripRevision;
    emit filmstripChanged();
}

QImage GifBridge::posterImage() const
{
    if (!m_core) {
        return QImage();
    }
    const GifCoreBuffer buffer = gif_poster_rgb(m_core);
    return imageFromRgb(buffer.data, static_cast<int>(gif_poster_width(m_core)),
                        static_cast<int>(gif_poster_height(m_core)));
}

QImage GifBridge::thumbImage(int index) const
{
    if (!m_core) {
        return QImage();
    }
    GifCoreFilmstrip strip = gif_filmstrip(m_core);
    if (index < 0 || static_cast<uint32_t>(index) >= strip.count) {
        return QImage();
    }
    const GifCoreThumb &thumb = strip.thumbs[index];
    const GifCoreBuffer buffer = gif_thumb_rgb(m_core, static_cast<uint32_t>(index));
    return imageFromRgb(buffer.data, static_cast<int>(thumb.width),
                        static_cast<int>(thumb.height));
}

QImage GifBridge::stripImage() const
{
    if (!m_core) {
        return QImage();
    }
    const GifCoreFilmstrip strip = gif_filmstrip(m_core);
    if (!strip.thumbs || strip.count == 0) {
        return QImage();
    }

    const int tileW = static_cast<int>(strip.thumbs[0].width);
    const int tileH = static_cast<int>(strip.thumbs[0].height);
    if (tileW <= 0 || tileH <= 0) {
        return QImage();
    }

    QImage image(tileW * static_cast<int>(strip.count), tileH, QImage::Format_RGB888);
    if (image.isNull()) {
        return QImage();
    }

    for (uint32_t i = 0; i < strip.count; ++i) {
        const GifCoreThumb &thumb = strip.thumbs[i];
        if (!thumb.data) {
            continue;
        }
        const int w = std::min(static_cast<int>(thumb.width), tileW);
        const int h = std::min(static_cast<int>(thumb.height), tileH);
        for (int y = 0; y < h; ++y) {
            std::memcpy(image.scanLine(y) + static_cast<size_t>(i) * tileW * 3,
                        thumb.data + static_cast<size_t>(y) * thumb.width * 3,
                        static_cast<size_t>(w) * 3);
        }
    }
    return image;
}

// ---------------------------------------------------------------------------
// Export settings
// ---------------------------------------------------------------------------

void GifBridge::setOutFps(double value)
{
    const double clamped = std::clamp(value, 1.0, 60.0);
    if (qFuzzyCompare(clamped, m_outFps)) {
        return;
    }
    m_outFps = clamped;
    emit settingsChanged();
    emit exportStateChanged();
}

void GifBridge::setOutWidth(int value)
{
    const int clamped = std::clamp(value, 0, 4096);
    if (clamped == m_outWidth) {
        return;
    }
    m_outWidth = clamped;
    emit settingsChanged();
}

void GifBridge::setOutColors(int value)
{
    const int clamped = std::clamp(value, 2, 256);
    if (clamped == m_outColors) {
        return;
    }
    m_outColors = clamped;
    emit settingsChanged();
}

void GifBridge::setDitherIndex(int value)
{
    const int clamped = std::clamp(value, 0, 2);
    if (clamped == m_ditherIndex) {
        return;
    }
    m_ditherIndex = clamped;
    emit settingsChanged();
}

void GifBridge::setSpeed(double value)
{
    const double clamped = std::clamp(value, 0.25, 4.0);
    if (qFuzzyCompare(clamped, m_speed)) {
        return;
    }
    m_speed = clamped;
    emit settingsChanged();
}

void GifBridge::setLoopForever(bool value)
{
    if (value == m_loopForever) {
        return;
    }
    m_loopForever = value;
    emit settingsChanged();
}

void GifBridge::setThreadCount(int value)
{
    // 0 is a real value here ("decide for me"), so clamp into [0, cpuCount]
    // rather than [1, cpuCount].
    const int clamped = std::clamp(value, 0, cpuCount());
    if (clamped == m_threadCount) {
        return;
    }
    m_threadCount = clamped;
    emit settingsChanged();
}

int GifBridge::cpuCount() const
{
    // The machine's core count is the kernel's business, not the UI's.
    const int count = static_cast<int>(gif_cpu_count());
    return count > 0 ? count : 1;
}

int GifBridge::defaultThreadCount() const
{
    const int count = static_cast<int>(gif_default_threads());
    return count > 0 ? count : 1;
}

int GifBridge::effectiveThreadCount() const
{
    const int count = static_cast<int>(gif_resolve_threads(static_cast<uint32_t>(m_threadCount)));
    return count > 0 ? count : 1;
}

QString GifBridge::threadSummary() const
{
    const int effective = effectiveThreadCount();
    if (m_threadCount == 0) {
        return QStringLiteral("自动（%1 线程，共 %2 核）").arg(effective).arg(cpuCount());
    }
    return QStringLiteral("%1 线程（共 %2 核）").arg(effective).arg(cpuCount());
}

// ---------------------------------------------------------------------------
// 高级设置
// ---------------------------------------------------------------------------

void GifBridge::setQuality(int value)
{
    const int clamped = std::clamp(value, 1, 10);
    if (clamped == m_quality) {
        return;
    }
    m_quality = clamped;
    emit settingsChanged();
}

void GifBridge::setLossy(int value)
{
    const int clamped = std::clamp(value, 0, 100);
    if (clamped == m_lossy) {
        return;
    }
    m_lossy = clamped;
    emit settingsChanged();
}

void GifBridge::setDitherStrength(int value)
{
    const int clamped = std::clamp(value, 0, 100);
    if (clamped == m_ditherStrength) {
        return;
    }
    m_ditherStrength = clamped;
    emit settingsChanged();
}

void GifBridge::setDedupe(bool value)
{
    if (value == m_dedupe) {
        return;
    }
    m_dedupe = value;
    emit settingsChanged();
}

void GifBridge::setOptimize(bool value)
{
    if (value == m_optimize) {
        return;
    }
    m_optimize = value;
    emit settingsChanged();
}

void GifBridge::setTolerance(int value)
{
    const int clamped = std::clamp(value, 0, 64);
    if (clamped == m_tolerance) {
        return;
    }
    m_tolerance = clamped;
    emit settingsChanged();
}

QString GifBridge::qualitySummary() const
{
    QString text = QStringLiteral("质量 %1/10").arg(m_quality);
    if (m_lossy > 0) {
        text += QStringLiteral(" · 有损 %1%").arg(m_lossy);
    }
    if (m_ditherStrength < 100) {
        text += QStringLiteral(" · 抖动 %1%").arg(m_ditherStrength);
    }
    if (m_dedupe) {
        text += QStringLiteral(" · 复用重复帧");
    }
    if (m_optimize) {
        text += QStringLiteral(" · 帧间透明");
    }
    return text;
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

void GifBridge::startExportTo(const QString &folder, const QString &fileName)
{
    const QString folderPath = QDir::toNativeSeparators(folder.trimmed());
    const QString name = fileName.trimmed();
    if (folderPath.isEmpty() || name.isEmpty()) {
        m_lastError = QStringLiteral("请先填写保存文件夹和文件名");
        emit resultChanged();
        return;
    }
    startExport(QDir(folderPath).filePath(ensureGifSuffix(name)));
}

void GifBridge::startExport(const QString &outputPath)
{
    // Every early return below reports through exportFinished. Callers wait on
    // that signal (the `--export` test hook quits when it arrives), so a silent
    // return left them waiting forever with the window still open.
    const auto refuse = [this](const QString &reason) {
        m_lastError = reason;
        m_hasResult = false;
        m_phaseText = QStringLiteral("失败");
        emit resultChanged();
        emit exportFinished(false, reason);
    };

    if (!m_core) {
        refuse(QStringLiteral("Rust 内核未初始化"));
        return;
    }
    if (m_exporting) {
        // A real export is already running and will emit when it ends; saying
        // anything here would report a failure that has not happened.
        return;
    }
    if (m_startExportWhenIdle) {
        // The same is true of one queued behind a size estimate.
        return;
    }
    if (!m_mediaLoaded) {
        refuse(QStringLiteral("请先打开一个视频"));
        return;
    }

    // The file dialog hands over a URL; turn it into a native path.
    QString requested = outputPath.trimmed();
    if (requested.startsWith(QLatin1String("file:"), Qt::CaseInsensitive)) {
        requested = QUrl(requested).toLocalFile();
    }
    requested = QDir::toNativeSeparators(requested);

    if (requested.isEmpty()) {
        refuse(QStringLiteral("请先选择 GIF 的保存位置"));
        return;
    }
    if (!requested.endsWith(QLatin1String(".gif"), Qt::CaseInsensitive)) {
        requested = ensureGifSuffix(requested);
    }

    gif_reset_progress(m_core);
    m_lastError.clear();
    m_hasResult = false;
    m_progress = 0.0;
    m_phaseText = QStringLiteral("准备中");
    emit resultChanged();
    emit progressChanged();

    // Freeze the settings. The worker must never read Qt state, and a start
    // deferred behind a size estimate must not pick up an edit made meanwhile.
    ExportRequest request;
    request.path = requested.toUtf8();
    request.dither = QByteArray(ditherName(m_ditherIndex));
    request.fps = m_outFps;
    request.width = m_outWidth > 0 ? m_outWidth : 4096;
    request.colors = m_outColors;
    request.speed = m_speed;
    request.loop = m_loopForever;
    request.threads = m_threadCount;
    request.quality = m_quality;
    request.lossy = m_lossy;
    request.ditherStrength = m_ditherStrength;
    request.dedupe = m_dedupe;
    request.optimize = m_optimize;
    request.tolerance = m_tolerance;

    if (m_estimateWorker.joinable()) {
        // A size estimate is decoding through the same kernel handle right now.
        gif_cancel(m_core);
        // Queue this instead of racing it; publishEstimate() (or onPollTick's
        // safety net) starts it. The poll timer is re-armed here because it is
        // the only path that publishes that estimate.
        m_startExportWhenIdle = [this, request]() { launchExport(request); };
        m_pollTimer.start();
        return;
    }

    launchExport(request);
}

void GifBridge::launchExport(const ExportRequest &request)
{
    m_workerDone.store(false);
    m_workerOk.store(false);
    {
        std::lock_guard<std::mutex> lock(m_resultMutex);
        m_workerMessage.clear();
    }

    m_exporting = true;
    emit exportStateChanged();

    m_worker = std::thread([this, request]() {
        GifCoreExportOptions options = {};
        options.fps = request.fps;
        options.max_width = static_cast<uint32_t>(request.width);
        options.colors = static_cast<uint32_t>(request.colors);
        options.dither = request.dither.constData();
        options.speed = request.speed;
        options.loop_forever = request.loop ? 1 : 0;
        options.threads = static_cast<uint32_t>(request.threads);
        options.quality = static_cast<uint32_t>(request.quality);
        options.lossy = static_cast<uint32_t>(request.lossy);
        options.dither_strength = static_cast<uint32_t>(request.ditherStrength);
        options.dedupe = request.dedupe ? 1 : 0;
        options.optimize = request.optimize ? 1 : 0;
        options.tolerance = static_cast<uint32_t>(request.tolerance);

        const gif_status status =
            gif_export_ex(m_core, request.path.constData(), &options);

        // gif_last_error is thread-local, so it must be read here rather than
        // on the timer that observes completion.
        QString message;
        if (status == GIF_OK) {
            m_workerOk.store(true);
            message = decodeError(gif_last_result_summary(m_core));
        } else if (status == GIF_ERR_OUTPUT) {
            // The kernel's own text is precise but terse; on its own it was just
            // "拒绝访问。 (os error 5)", which is what left the user asking why.
            // `sandboxRefusal` adds the sandbox explanation only when that is
            // actually the reason.
            const QString folder =
                QFileInfo(QString::fromUtf8(request.path)).absolutePath();
            message = QStringLiteral("保存失败：文件可能正被其他程序打开，"
                                     "或该文件夹没有写入权限。%1\n%2")
                          .arg(sandboxRefusal(folder),
                               decodeError(gif_last_error(m_core)));
        } else {
            message = decodeError(gif_last_error(m_core));
            if (message.isEmpty()) {
                message = QStringLiteral("导出失败");
            }
        }

        {
            std::lock_guard<std::mutex> lock(m_resultMutex);
            m_workerMessage = message;
        }
        m_workerDone.store(true);
    });

    m_pollTimer.start();
}

void GifBridge::cancelExport()
{
    if (!m_core || !m_exporting) {
        return;
    }
    gif_cancel(m_core);
    m_phaseText = QStringLiteral("正在取消");
    emit progressChanged();
}

void GifBridge::onPollTick()
{
    // Progress is the *export's* progress. An estimate writes the same counters
    // in the kernel, so it must never move the export bar.
    if (m_core && m_exporting) {
        const double value = gif_progress(m_core);
        const QString phase = decodeError(gif_progress_phase_name(m_core));
        if (!qFuzzyCompare(value + 1.0, m_progress + 1.0) || phase != m_phaseText) {
            m_progress = value;
            m_phaseText = phase;
            emit progressChanged();
        }
    }

    if (m_estimateWorker.joinable() && m_estimateDone.load()) {
        publishEstimate();
    }

    // A queued export is waiting for the estimate that was decoding on the same
    // kernel handle. publishEstimate() starts it, but if that estimate was
    // already published (or never ran) the queue would otherwise sit there for
    // ever, with 导出 silently doing nothing.
    if (m_startExportWhenIdle && !m_estimateWorker.joinable()) {
        std::function<void()> start;
        start.swap(m_startExportWhenIdle);
        start();
    }

    // Consume the export's completion exactly once. Leaving this flag set made
    // every later tick re-publish the previous result, and — far worse — stopped
    // the timer while an estimate was still decoding, which stranded the estimate
    // and any export queued behind it: 导出 then did nothing at all.
    if (m_workerDone.load()) {
        m_workerDone.store(false);
        if (m_worker.joinable()) {
            m_worker.join();
        }
        m_exporting = false;

        const bool ok = m_workerOk.load();
        QString message;
        {
            std::lock_guard<std::mutex> lock(m_resultMutex);
            message = m_workerMessage;
        }

        if (ok) {
            m_progress = 1.0;
            m_lastSummary = message;
            m_lastWarning = decodeError(m_core ? gif_last_warning(m_core) : nullptr);
            m_lastError.clear();
            m_hasResult = true;
            m_phaseText = QStringLiteral("已完成");
        } else {
            m_lastError = message;
            m_lastSummary.clear();
            m_lastWarning.clear();
            m_hasResult = false;
            m_phaseText = QStringLiteral("失败");
        }

        emit exportStateChanged();
        emit progressChanged();
        emit resultChanged();
        emit exportFinished(ok, message);

        // The settings may have moved while the export was running, and the bar
        // is free again now, so refresh the projection.
        scheduleEstimate();
    }

    // This timer is the only thing that publishes estimates and finishes
    // exports, so it has to stay alive while either is still in flight — a
    // running export (m_exporting), a finished worker whose result has not been
    // published yet, an estimate still decoding, or an export queued behind one.
    if (!m_exporting && !m_worker.joinable() && !m_estimateWorker.joinable()
        && !m_startExportWhenIdle) {
        m_pollTimer.stop();
    }
}

// ---------------------------------------------------------------------------
// 大小估计
// ---------------------------------------------------------------------------

void GifBridge::scheduleEstimate()
{
    if (!m_mediaLoaded) {
        clearEstimate();
        return;
    }
    // A running export owns the kernel handle; onPollTick re-schedules once it
    // is done. A queued export means the estimate must wait even longer.
    if (m_exporting || m_startExportWhenIdle) {
        return;
    }
    ++m_estimateGeneration;
    if (m_estimateWorker.joinable()) {
        gif_cancel(m_core);
    }
    m_estimateTimer.start();
}

void GifBridge::onEstimateDebounce()
{
    startEstimate();
}

void GifBridge::refreshEstimate()
{
    if (!m_mediaLoaded) {
        clearEstimate();
        return;
    }
    if (m_exporting || m_startExportWhenIdle) {
        return;
    }
    m_estimateTimer.stop();
    startEstimate();
}

void GifBridge::startEstimate()
{
    if (!m_core || !m_mediaLoaded || m_exporting || m_startExportWhenIdle) {
        return;
    }
    if (m_estimateWorker.joinable()) {
        // One is already decoding. It will publish and the debounce re-arms on
        // the next change, so there is nothing useful to do here.
        return;
    }

    GifCoreExportOptions options = {};
    options.fps = m_outFps;
    options.max_width = static_cast<uint32_t>(m_outWidth > 0 ? m_outWidth : 4096);
    options.colors = static_cast<uint32_t>(m_outColors);
    const QByteArray ditherUtf8 = QByteArray(ditherName(m_ditherIndex));
    options.dither = ditherUtf8.constData();
    options.speed = m_speed;
    options.loop_forever = m_loopForever ? 1 : 0;
    options.threads = static_cast<uint32_t>(m_threadCount);
    options.quality = static_cast<uint32_t>(m_quality);
    options.lossy = static_cast<uint32_t>(m_lossy);
    options.dither_strength = static_cast<uint32_t>(m_ditherStrength);
    options.dedupe = m_dedupe ? 1 : 0;
    options.optimize = m_optimize ? 1 : 0;
    options.tolerance = static_cast<uint32_t>(m_tolerance);

    ++m_estimateGeneration;
    const unsigned long long generation = m_estimateGeneration.load();

    m_estimateDone.store(false);
    m_estimating = true;
    emit estimateChanged();

    m_estimateWorker = std::thread([this, options, ditherUtf8, generation]() {
        GifCoreEstimate estimate = {};
        const gif_status status = gif_estimate_size(m_core, &options, &estimate);

        // gif_last_error is thread-local, so it has to be read on this thread.
        QString message;
        if (status != GIF_OK) {
            message = decodeError(gif_last_error(m_core));
            if (message.isEmpty()) {
                message = QStringLiteral("无法估算大小");
            }
        }

        {
            std::lock_guard<std::mutex> lock(m_estimateMutex);
            m_estimateResultGeneration = generation;
            if (generation != m_estimateGeneration.load() || status == GIF_ERR_CANCELLED) {
                m_estimateDone.store(true);
                return;
            }
            if (status == GIF_OK) {
                m_estimateText = formatBytes(estimate.bytes);
                m_estimateDetail =
                    QStringLiteral("%1 帧 · %2 × %3 · %4")
                        .arg(estimate.frames)
                        .arg(estimate.width)
                        .arg(estimate.height)
                        .arg(estimate.exact
                                 ? QStringLiteral("已完整测量")
                                 : QStringLiteral("抽样 %1 帧（%2 段）后推算")
                                       .arg(estimate.sampled)
                                       .arg(estimate.segments));
                m_estimateExact = estimate.exact != 0;
                m_estimateMessage.clear();
            } else {
                m_estimateText.clear();
                m_estimateDetail = message;
                m_estimateExact = false;
                m_estimateMessage = message;
            }
        }
        m_estimateDone.store(true);
    });

    m_pollTimer.start();
}

void GifBridge::publishEstimate()
{
    if (m_estimateWorker.joinable()) {
        m_estimateWorker.join();
    }

    // The poll timer is not stopped here: onPollTick() owns its lifetime and
    // keeps it alive while an export is queued or still running.

    // Superseded results are discarded; schedule the newest request after joining.
    bool rearm = false;
    {
        std::lock_guard<std::mutex> lock(m_estimateMutex);
        rearm = m_estimateResultGeneration != m_estimateGeneration.load();
    }

    m_estimating = false;
    emit estimateChanged();
    emit exportStateChanged();

    // An export clicked while this estimate ran is waiting for exactly this.
    if (m_startExportWhenIdle) {
        std::function<void()> start;
        start.swap(m_startExportWhenIdle);
        start();
        return;
    }
    if (rearm) {
        scheduleEstimate();
    }
}

void GifBridge::clearEstimate()
{
    m_estimateTimer.stop();
    if (m_estimateText.isEmpty() && m_estimateDetail.isEmpty() && !m_estimating) {
        return;
    }
    m_estimateText.clear();
    m_estimateDetail.clear();
    m_estimateExact = false;
    m_estimating = false;
    emit estimateChanged();
}

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

QString GifBridge::fileName() const
{
    if (m_videoPath.isEmpty()) {
        return QString();
    }
    return QFileInfo(m_videoPath).fileName();
}

QString GifBridge::resolutionText() const
{
    if (m_width <= 0 || m_height <= 0) {
        return QStringLiteral("—");
    }
    return QStringLiteral("%1 × %2").arg(m_width).arg(m_height);
}

QString GifBridge::fpsText() const
{
    if (m_fps <= 0.0) {
        return QStringLiteral("—");
    }
    const double rounded = std::round(m_fps * 100.0) / 100.0;
    if (qFuzzyCompare(rounded, std::round(rounded))) {
        return QStringLiteral("%1").arg(static_cast<qint64>(std::round(rounded)));
    }
    return QString::number(rounded, 'f', 2);
}

QString GifBridge::durationText() const
{
    return formatTime(m_duration);
}

QString GifBridge::frameCountText() const
{
    if (m_frameCount > 0) {
        return QString::number(static_cast<qulonglong>(m_frameCount));
    }
    if (!m_core) {
        return QStringLiteral("0");
    }
    return QString::number(static_cast<qulonglong>(gif_total_frames(m_core)));
}

QString GifBridge::mediaSummary() const
{
    if (!m_mediaLoaded) {
        return QStringLiteral("尚未载入视频");
    }
    return QStringLiteral("%1 · %2 · %3 fps · %4")
        .arg(resolutionText(), durationText(), fpsText(), frameCountText());
}
