#include "GifBridge.h"

#include <QDateTime>
#include <QFile>
#include <QGuiApplication>
#include <QImage>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QString>
#include <QStringList>
#include <QTextStream>
#include <QTimer>

namespace {

/// Command line switches. Everything is optional; with none of them the program
/// is a normal windowed application.
///
///   --open <video>        load this file at start-up
///   --screenshot <png>    render the window once, save it, and exit
///   --export <gif>        run an export through the bridge, then exit
///   --delay <ms>          how long to wait before grabbing (default 1800)
///   --threads <n>         export worker count (0 = let the kernel decide)
///   --log <file>          append what happened to this file
///
/// `--screenshot` and `--export` exist so the UI can be checked without a human
/// watching it; they change nothing about the normal interactive path. `--log`
/// is what makes them debuggable: this is a GUI-subsystem binary, so it has no
/// console and anything written to stderr is simply lost.
struct Options
{
    QString open;
    QString screenshot;
    QString exportTo;
    QString log;
    int delay = 1800;
    /// -1 leaves the bridge's own default (0 = auto) alone.
    int threads = -1;
};

/// Append one line to the log file, if one was asked for. Failures are ignored:
/// a log that cannot be written must never take the program down with it.
void appendLog(const QString &path, const QString &line)
{
    if (path.isEmpty()) {
        return;
    }
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly | QIODevice::Append | QIODevice::Text)) {
        return;
    }
    QTextStream out(&file);
    out << QDateTime::currentDateTime().toString(Qt::ISODate) << ' ' << line
        << '\n';
}

Options parseArgs(const QStringList &args)
{
    Options o;
    for (int i = 1; i < args.size(); ++i) {
        const QString a = args.at(i);
        const bool hasNext = i + 1 < args.size();
        if (a == QLatin1String("--open") && hasNext) {
            o.open = args.at(++i);
        } else if (a == QLatin1String("--screenshot") && hasNext) {
            o.screenshot = args.at(++i);
        } else if (a == QLatin1String("--export") && hasNext) {
            o.exportTo = args.at(++i);
        } else if (a == QLatin1String("--log") && hasNext) {
            o.log = args.at(++i);
        } else if (a == QLatin1String("--delay") && hasNext) {
            o.delay = qMax(0, args.at(++i).toInt());
        } else if (a == QLatin1String("--threads") && hasNext) {
            o.threads = qMax(0, args.at(++i).toInt());
        }
    }
    return o;
}

} // namespace

int main(int argc, char *argv[])
{
    // Pin the control style so the look never depends on a style plugin that
    // happens to be installed on the machine. This must run before the
    // QGuiApplication exists: Qt otherwise resolves a platform default style
    // first, which would defeat both the pinning and the pruning of the unused
    // style plugins from the bundle.
    QQuickStyle::setStyle(QStringLiteral("Basic"));

    QGuiApplication app(argc, argv);
    app.setApplicationName(QStringLiteral("Easy2GIF"));
    app.setApplicationDisplayName(QStringLiteral("Easy2GIF"));
    app.setOrganizationName(QStringLiteral("GifCore"));
    app.setApplicationVersion(QStringLiteral("1.0.0"));

    const Options options = parseArgs(app.arguments());

    GifBridge bridge;

    QQmlApplicationEngine engine;
    engine.addImageProvider(QStringLiteral("gifcore"),
                            new GifImageProvider(&bridge));
    engine.rootContext()->setContextProperty(QStringLiteral("gif"), &bridge);

    QObject::connect(
        &engine, &QQmlApplicationEngine::objectCreationFailed, &app,
        []() { QCoreApplication::exit(-1); }, Qt::QueuedConnection);

    engine.loadFromModule("GifForge", "Main");
    if (engine.rootObjects().isEmpty()) {
        return -1;
    }

    // Report the health of the bundled media tools once the window exists, so a
    // missing binary surfaces in the UI instead of at the first export.
    bridge.checkTools();

    if (options.threads >= 0) {
        bridge.setThreadCount(options.threads);
    }

    if (!options.open.isEmpty()) {
        bridge.openVideo(options.open);
    }

    if (!options.log.isEmpty()) {
        appendLog(options.log,
                  QStringLiteral("tools ready=%1 (%2)")
                      .arg(bridge.toolsReady() ? 1 : 0)
                      .arg(bridge.toolsStatus()));
        appendLog(options.log,
                  QStringLiteral("media loaded=%1 error=%2")
                      .arg(bridge.mediaLoaded() ? 1 : 0)
                      .arg(bridge.lastError()));
    }

    if (!options.screenshot.isEmpty() || !options.exportTo.isEmpty()) {
        auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
        if (!window) {
            appendLog(options.log, QStringLiteral("no root window"));
            return -2;
        }

        if (!options.exportTo.isEmpty()) {
            QObject::connect(
                &bridge, &GifBridge::exportFinished, &app,
                [&app, &options](bool ok, const QString &message) {
                    qInfo("export %s: %s", ok ? "ok" : "failed",
                          qUtf8Printable(message));
                    appendLog(options.log,
                              QStringLiteral("export %1: %2")
                                  .arg(ok ? QStringLiteral("ok")
                                          : QStringLiteral("FAILED"))
                                  .arg(message));
                    QTimer::singleShot(0, &app,
                                       [&app, ok] { app.exit(ok ? 0 : 3); });
                });
            QTimer::singleShot(options.delay, &bridge, [&bridge, &options] {
                bridge.startExport(options.exportTo);
            });
            return app.exec();
        }

        // Wait for the scene graph to settle before grabbing, then write the
        // frame out and stop.
        QTimer::singleShot(options.delay, &app, [&app, window, &options] {
            const QImage shot = window->grabWindow();
            const bool saved = !shot.isNull() && shot.save(options.screenshot);
            qInfo("screenshot %s %dx%d", saved ? "ok" : "FAILED", shot.width(),
                  shot.height());
            appendLog(options.log,
                      QStringLiteral("screenshot %1 %2x%3")
                          .arg(saved ? QStringLiteral("ok")
                                     : QStringLiteral("FAILED"))
                          .arg(shot.width())
                          .arg(shot.height()));
            QTimer::singleShot(0, &app, [&app, saved] { app.exit(saved ? 0 : 4); });
        });
    }

    return app.exec();
}
