#pragma once

// GifBridge — the Qt-facing façade over the Rust kernel's C ABI.
//
// This class is deliberately a *presenter*: it holds no video logic. Every
// question about the media and every byte of pixel data comes from the kernel
// through gifcore.h, and every command is forwarded straight back. The only
// work done here is Qt-shaped glue — turning a raw RGB24 buffer into a QImage
// the scene graph can draw, formatting numbers for labels, and running blocking imports, exports and size estimates off the GUI thread.

#include <QByteArray>
#include <QImage>
#include <QObject>
#include <QQuickImageProvider>
#include <QString>
#include <QTimer>

#include <atomic>
#include <functional>
#include <mutex>
#include <thread>

struct GifCore;

class GifBridge;

/// Serves the poster frame, individual thumbnails and the composited timeline
/// strip to QML as `image://gifcore/<id>`.
class GifImageProvider : public QQuickImageProvider
{
public:
    explicit GifImageProvider(GifBridge *bridge);

    QImage requestImage(const QString &id, QSize *size,
                        const QSize &requestedSize) override;

private:
    GifBridge *m_bridge;
};

class GifBridge : public QObject
{
    Q_OBJECT

    // --- media -----------------------------------------------------------
    Q_PROPERTY(bool mediaLoaded READ mediaLoaded NOTIFY mediaChanged)
    Q_PROPERTY(bool importing READ importing NOTIFY importChanged)
    Q_PROPERTY(double importProgress READ importProgress NOTIFY importChanged)
    Q_PROPERTY(QString importPhase READ importPhase NOTIFY importChanged)
    Q_PROPERTY(QString importFileName READ importFileName NOTIFY importChanged)
    Q_PROPERTY(QString videoPath READ videoPath NOTIFY mediaChanged)
    Q_PROPERTY(QString fileName READ fileName NOTIFY mediaChanged)
    Q_PROPERTY(QString videoCodec READ videoCodec NOTIFY mediaChanged)
    Q_PROPERTY(bool hasAudio READ hasAudio NOTIFY mediaChanged)
    Q_PROPERTY(QString resolutionText READ resolutionText NOTIFY mediaChanged)
    Q_PROPERTY(QString fpsText READ fpsText NOTIFY mediaChanged)
    Q_PROPERTY(QString durationText READ durationText NOTIFY mediaChanged)
    Q_PROPERTY(QString frameCountText READ frameCountText NOTIFY mediaChanged)
    Q_PROPERTY(double duration READ duration NOTIFY mediaChanged)
    Q_PROPERTY(QString mediaSummary READ mediaSummary NOTIFY mediaChanged)

    // --- selection -------------------------------------------------------
    Q_PROPERTY(double startTime READ startTime NOTIFY selectionChanged)
    Q_PROPERTY(double endTime READ endTime NOTIFY selectionChanged)
    Q_PROPERTY(double selectionDuration READ selectionDuration NOTIFY selectionChanged)
    Q_PROPERTY(double startFraction READ startFraction NOTIFY selectionChanged)
    Q_PROPERTY(double endFraction READ endFraction NOTIFY selectionChanged)
    Q_PROPERTY(QString startTimeText READ startTimeText NOTIFY selectionChanged)
    Q_PROPERTY(QString endTimeText READ endTimeText NOTIFY selectionChanged)
    Q_PROPERTY(QString selectionDurationText READ selectionDurationText NOTIFY selectionChanged)
    Q_PROPERTY(QString startFrameText READ startFrameText NOTIFY selectionChanged)
    Q_PROPERTY(QString endFrameText READ endFrameText NOTIFY selectionChanged)
    Q_PROPERTY(QString selectionFramesText READ selectionFramesText NOTIFY selectionChanged)

    // --- preview ---------------------------------------------------------
    Q_PROPERTY(int posterRevision READ posterRevision NOTIFY posterChanged)
    Q_PROPERTY(int filmstripRevision READ filmstripRevision NOTIFY filmstripChanged)
    Q_PROPERTY(int filmstripCount READ filmstripCount NOTIFY filmstripChanged)
    Q_PROPERTY(bool filmstripReady READ filmstripReady NOTIFY filmstripChanged)

    // --- export settings -------------------------------------------------
    Q_PROPERTY(double outFps READ outFps WRITE setOutFps NOTIFY settingsChanged)
    Q_PROPERTY(int outWidth READ outWidth WRITE setOutWidth NOTIFY settingsChanged)
    Q_PROPERTY(int outColors READ outColors WRITE setOutColors NOTIFY settingsChanged)
    Q_PROPERTY(int ditherIndex READ ditherIndex WRITE setDitherIndex NOTIFY settingsChanged)
    Q_PROPERTY(double speed READ speed WRITE setSpeed NOTIFY settingsChanged)
    Q_PROPERTY(bool loopForever READ loopForever WRITE setLoopForever NOTIFY settingsChanged)
    Q_PROPERTY(int threadCount READ threadCount WRITE setThreadCount NOTIFY settingsChanged)
    Q_PROPERTY(int cpuCount READ cpuCount NOTIFY settingsChanged)
    Q_PROPERTY(int defaultThreadCount READ defaultThreadCount NOTIFY settingsChanged)
    Q_PROPERTY(int effectiveThreadCount READ effectiveThreadCount NOTIFY settingsChanged)
    Q_PROPERTY(QString threadSummary READ threadSummary NOTIFY settingsChanged)
    Q_PROPERTY(QString estimatedFramesText READ estimatedFramesText NOTIFY exportStateChanged)

    // --- 高级设置 ---------------------------------------------------------
    // Every one of these defaults to the behaviour that existed before it did,
    // so an untouched export stays byte-for-byte what it always was.
    /// 编码质量 1..=10: higher spends more k-means iterations on the palette.
    Q_PROPERTY(int quality READ quality WRITE setQuality NOTIFY settingsChanged)
    /// 有损压缩 0..=100: snap colours to a coarser grid before quantising.
    Q_PROPERTY(int lossy READ lossy WRITE setLossy NOTIFY settingsChanged)
    /// 抖动强度 0..=100; 100 is the historic amplitude, 0 maps flat.
    Q_PROPERTY(int ditherStrength READ ditherStrength WRITE setDitherStrength NOTIFY settingsChanged)
    /// 复用重复帧: hold identical frames longer instead of writing them twice.
    Q_PROPERTY(bool dedupe READ dedupe WRITE setDedupe NOTIFY settingsChanged)
    /// 帧间透明优化: leave pixels that did not change transparent.
    Q_PROPERTY(bool optimize READ optimize WRITE setOptimize NOTIFY settingsChanged)
    /// Per-channel slack 0..=64 used by 帧间透明优化.
    Q_PROPERTY(int tolerance READ tolerance WRITE setTolerance NOTIFY settingsChanged)
    /// One-line description of the advanced settings, for the UI.
    Q_PROPERTY(QString qualitySummary READ qualitySummary NOTIFY settingsChanged)

    // --- 大小估计（异步，绝不阻塞界面） -----------------------------------
    Q_PROPERTY(QString estimateText READ estimateText NOTIFY estimateChanged)
    Q_PROPERTY(QString estimateDetailText READ estimateDetailText NOTIFY estimateChanged)
    Q_PROPERTY(bool estimating READ estimating NOTIFY estimateChanged)
    Q_PROPERTY(bool estimateExact READ estimateExact NOTIFY estimateChanged)

    // --- where the GIF goes ----------------------------------------------
    // Kept as bridge state, not loose QML properties, so the write check runs
    // once per change instead of once per read.
    Q_PROPERTY(QString outputDir READ outputDir WRITE setOutputDir NOTIFY outputChanged)
    Q_PROPERTY(QString outputName READ outputName WRITE setOutputName NOTIFY outputChanged)
    Q_PROPERTY(QString outputTargetText READ outputTargetText NOTIFY outputChanged)
    Q_PROPERTY(QString outputProblemText READ outputProblemText NOTIFY outputChanged)

    // --- export progress -------------------------------------------------
    Q_PROPERTY(bool exporting READ exporting NOTIFY exportStateChanged)
    Q_PROPERTY(double progress READ progress NOTIFY progressChanged)
    Q_PROPERTY(QString phaseText READ phaseText NOTIFY progressChanged)
    Q_PROPERTY(QString lastSummary READ lastSummary NOTIFY resultChanged)
    Q_PROPERTY(QString lastWarning READ lastWarning NOTIFY resultChanged)
    Q_PROPERTY(QString lastError READ lastError NOTIFY resultChanged)
    Q_PROPERTY(bool hasResult READ hasResult NOTIFY resultChanged)

    // --- environment -----------------------------------------------------
    Q_PROPERTY(QString coreVersion READ coreVersion CONSTANT)
    Q_PROPERTY(QString toolsStatus READ toolsStatus NOTIFY toolsChanged)
    Q_PROPERTY(bool toolsReady READ toolsReady NOTIFY toolsChanged)
    Q_PROPERTY(bool darkMode READ darkMode WRITE setDarkMode NOTIFY themeChanged)
    /// True when this process runs at Low integrity, i.e. it was started by a
    /// sandboxed or otherwise restricted launcher. Windows then refuses every
    /// write to an ordinary folder whatever its ACL says, so the UI says so up
    /// front instead of blaming the folder the user picked.
    Q_PROPERTY(bool sandboxed READ sandboxed CONSTANT)
    /// "低完整性（沙箱）" / "普通", for the header chip.
    Q_PROPERTY(QString sandboxText READ sandboxText CONSTANT)

public:
    explicit GifBridge(QObject *parent = nullptr);
    ~GifBridge() override;

    // QML commands --------------------------------------------------------
    /// Starts a background import; true means accepted, not yet completed.
    Q_INVOKABLE bool openVideo(const QString &pathOrUrl);
    Q_INVOKABLE void closeVideo();

    Q_INVOKABLE void setStartTime(double seconds);
    Q_INVOKABLE void setEndTime(double seconds);
    Q_INVOKABLE void setStartFraction(double fraction);
    Q_INVOKABLE void setEndFraction(double fraction);
    Q_INVOKABLE void nudgeStart(double deltaSeconds);
    Q_INVOKABLE void nudgeEnd(double deltaSeconds);
    Q_INVOKABLE void selectWholeClip();
    Q_INVOKABLE void clearSelection();

    /// Accepts "83.5", "1:23", "1:02:03.500" or "150f" (a frame index).
    Q_INVOKABLE double parseTime(const QString &text) const;
    Q_INVOKABLE QString formatTime(double seconds) const;

    Q_INVOKABLE void buildFilmstrip(int count, int thumbWidth);
    Q_INVOKABLE void refreshPoster();

    Q_INVOKABLE bool checkTools();
    Q_INVOKABLE QString defaultOutputPath() const;

    /// Folder and file name the export form starts with: the video sits next to
    /// its source, the name is the source's base name with `.gif`.
    Q_INVOKABLE QString defaultOutputDir() const;
    Q_INVOKABLE QString defaultOutputName() const;

    /// Ask the kernel whether the GIF could be written to `folder/fileName`.
    /// Returns an empty string when it can, otherwise a message ready to show.
    /// Nothing is created except a short-lived probe file.
    Q_INVOKABLE QString outputProblem(const QString &folder, const QString &fileName) const;

    /// Export to `folder/fileName`, composed here so QML never joins paths.
    Q_INVOKABLE void startExportTo(const QString &folder, const QString &fileName);

    /// Turn a `file:` URL (or an already-native path) into a native path. Used
    /// for the folder picker, which hands over a URL.
    Q_INVOKABLE QString toLocalPath(const QString &urlOrPath) const;

    Q_INVOKABLE void startExport(const QString &outputPath);
    Q_INVOKABLE void cancelExport();

    /// Run the size estimate now instead of waiting for the debounce to expire.
    Q_INVOKABLE void refreshEstimate();

    // Image-provider support ---------------------------------------------
    QImage posterImage() const;
    QImage thumbImage(int index) const;
    QImage stripImage() const;

    // Accessors used by the Q_PROPERTY declarations ----------------------
    /// Read once: the integrity level is a property of the token this process
    /// was started with and cannot change while it runs.
    bool sandboxed() const;
    QString sandboxText() const;

    QString outputDir() const { return m_outputDir; }
    void setOutputDir(const QString &dir);
    QString outputName() const { return m_outputName; }
    void setOutputName(const QString &name);
    /// The resolved native path the export would write to.
    QString outputTargetText() const;
    /// Empty when the target is writable, otherwise a message ready to show.
    QString outputProblemText() const { return m_outputProblem; }

    bool mediaLoaded() const { return m_mediaLoaded; }
    QString videoPath() const { return m_videoPath; }
    QString fileName() const;
    QString videoCodec() const { return m_videoCodec; }
    bool hasAudio() const { return m_hasAudio; }
    QString resolutionText() const;
    QString fpsText() const;
    QString durationText() const;
    QString frameCountText() const;
    double duration() const { return m_duration; }
    QString mediaSummary() const;

    double startTime() const { return m_start; }
    double endTime() const { return m_end; }
    double selectionDuration() const;
    double startFraction() const;
    double endFraction() const;
    QString startTimeText() const { return formatTime(m_start); }
    QString endTimeText() const { return formatTime(m_end); }
    QString selectionDurationText() const { return formatTime(selectionDuration()); }
    QString startFrameText() const;
    QString endFrameText() const;
    QString selectionFramesText() const;

    int posterRevision() const { return m_posterRevision; }
    int filmstripRevision() const { return m_filmstripRevision; }
    int filmstripCount() const { return m_filmstripCount; }
    bool filmstripReady() const { return m_filmstripCount > 0; }

    double outFps() const { return m_outFps; }
    void setOutFps(double v);
    int outWidth() const { return m_outWidth; }
    void setOutWidth(int v);
    int outColors() const { return m_outColors; }
    void setOutColors(int v);
    int ditherIndex() const { return m_ditherIndex; }
    void setDitherIndex(int v);
    double speed() const { return m_speed; }
    void setSpeed(double v);
    bool loopForever() const { return m_loopForever; }
    void setLoopForever(bool v);
    /// Requested worker count; 0 means "decide for me" (half the CPUs).
    int threadCount() const { return m_threadCount; }
    void setThreadCount(int v);
    /// Logical CPUs on this machine, straight from the kernel.
    int cpuCount() const;
    /// What a request of 0 resolves to.
    int defaultThreadCount() const;
    /// What the current request will actually resolve to.
    int effectiveThreadCount() const;
    /// Human-readable description of the current choice, for the UI.
    QString threadSummary() const;
    QString estimatedFramesText() const;

    int quality() const { return m_quality; }
    void setQuality(int v);
    int lossy() const { return m_lossy; }
    void setLossy(int v);
    int ditherStrength() const { return m_ditherStrength; }
    void setDitherStrength(int v);
    bool dedupe() const { return m_dedupe; }
    void setDedupe(bool v);
    bool optimize() const { return m_optimize; }
    void setOptimize(bool v);
    int tolerance() const { return m_tolerance; }
    void setTolerance(int v);
    QString qualitySummary() const;

    QString estimateText() const { return m_estimateText; }
    QString estimateDetailText() const { return m_estimateDetail; }
    bool estimating() const { return m_estimating; }
    bool estimateExact() const { return m_estimateExact; }

    bool exporting() const { return m_exporting; }
    double progress() const { return m_progress; }
    QString phaseText() const { return m_phaseText; }
    QString lastSummary() const { return m_lastSummary; }
    QString lastWarning() const { return m_lastWarning; }
    QString lastError() const { return m_lastError; }
    bool hasResult() const { return m_hasResult; }

    QString coreVersion() const { return m_coreVersion; }
    QString toolsStatus() const { return m_toolsStatus; }
    bool toolsReady() const { return m_toolsReady; }
    bool darkMode() const { return m_darkMode; }
    void setDarkMode(bool dark);
    bool importing() const { return m_importing; }
    double importProgress() const { return m_importStage / 3.0; }
    QString importPhase() const;
    QString importFileName() const;

signals:
    void mediaChanged();
    void importChanged();
    void importFinished(bool success);
    void selectionChanged();
    void posterChanged();
    void filmstripChanged();
    void settingsChanged();
    void exportStateChanged();
    void progressChanged();
    void resultChanged();
    void toolsChanged();
    void themeChanged();
    /// The save folder or file name changed, so the target and its write check
    /// both moved.
    void outputChanged();
    void errorOccurred(const QString &message);
    void exportFinished(bool success, const QString &message);
    /// The projected size, or the fact that it is being worked out, changed.
    void estimateChanged();

private slots:
    void onPollTick();
    void onPosterDebounce();
    void onEstimateDebounce();

private:
    /// The settings frozen the moment 导出 was pressed. Plain values only, so a
    /// start deferred behind a size estimate cannot pick up a later edit.
    struct ExportRequest {
        QByteArray path;
        QByteArray dither;
        double fps = 12.0;
        int width = 480;
        int colors = 256;
        double speed = 1.0;
        bool loop = true;
        int threads = 0;
        int quality = 5;
        int lossy = 0;
        int ditherStrength = 100;
        bool dedupe = false;
        bool optimize = false;
        int tolerance = 0;
    };

    /// Human-readable message for the last kernel call, or an empty string.
    QString takeError() const;
    void refreshMediaFromKernel();
    void pushSelectionToKernel();
    void fail(const QString &context);
    /// Re-run the kernel's write check for the current folder and name.
    void refreshOutputCheck();

    /// Hand a frozen request to the kernel on a worker thread. Never called
    /// while a size estimate is running on the same handle.
    void launchExport(const ExportRequest &request);

    /// Restart the estimate debounce; called by everything that can change the
    /// projected size. Never touches the kernel directly.
    void scheduleEstimate();
    /// Start a worker that fills in the projection, unless one is already
    /// running or an export is in flight.
    void startEstimate();
    /// Publish whatever the estimator worker produced, if it is still current.
    void publishEstimate();
    /// Drop the projection, e.g. when the video is closed.
    void clearEstimate();
    void publishImport();

    GifCore *m_core = nullptr;
    bool m_darkMode = false;

    // Import into a separate handle so the current media and borrowed preview
    // buffers remain untouched until every background operation has finished.
    bool m_importing = false;
    QString m_importPath;
    int m_importStage = 0;
    GifCore *m_importCore = nullptr;
    std::thread m_importWorker;
    std::atomic<int> m_importWorkerStage{0};
    std::atomic<bool> m_importDone{false};
    bool m_importOk = false; // read only after joining m_importWorker
    QString m_importError;
    QString m_importWarning;

    // media snapshot
    bool m_mediaLoaded = false;
    QString m_videoPath;
    QString m_videoCodec;
    bool m_hasAudio = false;
    int m_width = 0;
    int m_height = 0;
    double m_fps = 0.0;
    double m_duration = 0.0;
    unsigned long long m_frameCount = 0;

    // selection (seconds)
    double m_start = 0.0;
    double m_end = 0.0;

    // preview
    int m_posterRevision = 0;
    int m_filmstripRevision = 0;
    int m_filmstripCount = 0;
    QTimer m_posterTimer;

    // settings
    double m_outFps = 12.0;
    int m_outWidth = 480;
    int m_outColors = 256;
    int m_ditherIndex = 2;
    double m_speed = 1.0;
    bool m_loopForever = true;
    /// 0 asks the kernel for its default (half the machine's logical CPUs).
    int m_threadCount = 0;

    // 高级设置. The defaults are the historical behaviour, so a fresh profile
    // produces exactly the GIF the app produced before these existed.
    int m_quality = 5;
    int m_lossy = 0;
    int m_ditherStrength = 100;
    bool m_dedupe = false;
    bool m_optimize = false;
    int m_tolerance = 0;

    // 大小估计: a debounced worker so the window never waits on it.
    QString m_estimateText;
    QString m_estimateDetail;
    bool m_estimating = false;
    bool m_estimateExact = false;
    /// Bumped for every measurement asked for; the worker stamps the value it
    /// was started with, so a result that arrived after the settings moved can
    /// be published *and* followed by a fresh one.
    std::atomic<unsigned long long> m_estimateGeneration{0};
    QTimer m_estimateTimer;
    std::thread m_estimateWorker;
    std::atomic<bool> m_estimateDone{false};
    mutable std::mutex m_estimateMutex;
    QString m_estimateMessage;
    unsigned long long m_estimateResultGeneration = 0; // guarded by m_estimateMutex

    // where the GIF goes
    QString m_outputDir;
    QString m_outputName;
    QString m_outputProblem;

    // export
    bool m_exporting = false;
    double m_progress = 0.0;
    QString m_phaseText;
    QString m_lastSummary;
    QString m_lastWarning;
    QString m_lastError;
    bool m_hasResult = false;

    QTimer m_pollTimer;
    std::thread m_worker;
    std::atomic<bool> m_workerDone{false};
    std::atomic<bool> m_workerOk{false};
    mutable std::mutex m_resultMutex;
    QString m_workerMessage;
    /// Set when 导出 was pressed while a size estimate still held the kernel
    /// handle; it runs as soon as that estimate is done. Its presence also means
    /// "an export is already queued", so a second click cannot queue another.
    std::function<void()> m_startExportWhenIdle;

    // environment
    QString m_coreVersion;
    QString m_toolsStatus;
    bool m_toolsReady = false;

    friend class GifImageProvider;
};
