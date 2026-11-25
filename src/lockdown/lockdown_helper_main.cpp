#include "keyboard_blocker.h"
#include "lockdown_protocol.h"

#include <QCoreApplication>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QHash>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSet>
#include <QSocketNotifier>
#include <QTimer>

#include <cerrno>
#include <csignal>
#include <cstdio>
#include <fcntl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <unistd.h>

namespace {

int gSignalPipe[2] = {-1, -1};

void posixSignalHandler(int)
{
    if (gSignalPipe[0] >= 0) {
        const char byte = 1;
        const ssize_t ignored = ::write(gSignalPipe[0], &byte, sizeof(byte));
        Q_UNUSED(ignored);
    }
}

void writeJsonLine(const QJsonObject &obj)
{
    QByteArray line = QJsonDocument(obj).toJson(QJsonDocument::Compact);
    line.append('\n');
    const ssize_t ignored = ::write(STDOUT_FILENO, line.constData(), static_cast<std::size_t>(line.size()));
    Q_UNUSED(ignored);
}

QString readTrimmedFile(const QString &path)
{
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) {
        return {};
    }
    return QString::fromUtf8(file.readAll()).trimmed();
}

qint64 readParentPid(qint64 pid)
{
    const QString statText = readTrimmedFile(QStringLiteral("/proc/%1/stat").arg(pid));
    const qsizetype closeParen = statText.lastIndexOf(QLatin1Char(')'));
    if (closeParen < 0) {
        return 0;
    }
    const QStringList fields = statText.mid(closeParen + 2).split(QLatin1Char(' '), Qt::SkipEmptyParts);
    // fields[0] = state, fields[1] = ppid
    if (fields.size() < 2) {
        return 0;
    }
    bool ok = false;
    const qint64 ppid = fields.at(1).toLongLong(&ok);
    return ok ? ppid : 0;
}

bool isDescendantOf(qint64 pid, qint64 ancestorPid, QHash<qint64, qint64> &ppidCache)
{
    if (ancestorPid <= 0 || pid <= 0) {
        return false;
    }
    qint64 current = pid;
    for (int depth = 0; depth < 32 && current > 1; ++depth) {
        if (current == ancestorPid) {
            return true;
        }
        auto it = ppidCache.constFind(current);
        if (it == ppidCache.cend()) {
            const qint64 parent = readParentPid(current);
            it = ppidCache.insert(current, parent);
        }
        if (it.value() == current || it.value() <= 1) {
            break;
        }
        current = it.value();
    }
    return false;
}

QStringList scanProhibitedProcesses(const QStringList &blacklist, qint64 sebPid)
{
    QSet<QString> prohibited;
    for (const QString &entry : blacklist) {
        const QString name = QFileInfo(entry.trimmed()).fileName().toLower();
        if (!name.isEmpty()) {
            prohibited.insert(name);
        }
    }
    if (prohibited.isEmpty()) {
        return {};
    }

    const qint64 selfPid = static_cast<qint64>(::getpid());
    const qint64 directParentPid = static_cast<qint64>(::getppid());
    QHash<qint64, qint64> ppidCache;
    QStringList detected;
    QSet<QString> seen;

    const QDir procDir(QStringLiteral("/proc"));
    const QStringList entries = procDir.entryList(QDir::Dirs | QDir::NoDotAndDotDot);
    for (const QString &entry : entries) {
        bool ok = false;
        const qint64 pid = entry.toLongLong(&ok);
        if (!ok || pid <= 1 || pid == selfPid || pid == directParentPid || pid == sebPid) {
            continue;
        }
        if (sebPid > 1 && isDescendantOf(pid, sebPid, ppidCache)) {
            continue;
        }

        QString exePath = QFileInfo(QStringLiteral("/proc/%1/exe").arg(pid)).symLinkTarget();
        if (exePath.endsWith(QStringLiteral(" (deleted)")) && !QFileInfo::exists(exePath)) {
            exePath.chop(10);
        }
        const QString exeName = QFileInfo(exePath).fileName().toLower();
        const QString commName = readTrimmedFile(QStringLiteral("/proc/%1/comm").arg(pid)).toLower();

        QString matched;
        if (!exeName.isEmpty() && prohibited.contains(exeName)) {
            matched = exeName;
        } else if (!commName.isEmpty() && prohibited.contains(commName)) {
            matched = commName;
        }

        if (!matched.isEmpty()) {
            const QString label = QStringLiteral("%1 (PID %2)").arg(matched).arg(pid);
            if (!seen.contains(label)) {
                seen.insert(label);
                detected.append(label);
            }
        }
    }

    detected.sort(Qt::CaseInsensitive);
    return detected;
}

class LockdownHelperController : public QObject
{
public:
    explicit LockdownHelperController(QObject *parent = nullptr)
        : QObject(parent)
        , keyboard_(this)
    {
        watchdogTimer_.setSingleShot(true);
        watchdogTimer_.setInterval(seb::lockdown::kWatchdogTimeoutMs);
        connect(&watchdogTimer_, &QTimer::timeout, this, [this] {
            if (locked_) {
                keyboard_.unlock();
                locked_ = false;
                writeJsonLine({
                    {QStringLiteral("event"), QStringLiteral("error")},
                    {QStringLiteral("message"), QStringLiteral("Watchdog timeout; released lockdown safely.")},
                });
                QCoreApplication::quit();
            }
        });

        monitorTimer_.setSingleShot(false);
        monitorTimer_.setInterval(seb::lockdown::kHeartbeatIntervalMs);
        connect(&monitorTimer_, &QTimer::timeout, this, [this] {
            if (!locked_) {
                return;
            }
            const QStringList found = scanProhibitedProcesses(blacklist_, sebPid_);
            if (!found.isEmpty()) {
                writeJsonLine({
                    {QStringLiteral("event"), QStringLiteral("violation")},
                    {QStringLiteral("reason"),
                     QStringLiteral("Prohibited applications detected during exam: %1").arg(found.join(QStringLiteral(", ")))},
                    {QStringLiteral("prohibited"), seb::lockdown::toJsonArray(found)},
                });
            }
        });

        const int flags = ::fcntl(STDIN_FILENO, F_GETFL, 0);
        if (flags >= 0) {
            ::fcntl(STDIN_FILENO, F_SETFL, flags | O_NONBLOCK);
        }

        stdinNotifier_ = new QSocketNotifier(STDIN_FILENO, QSocketNotifier::Read, this);
        connect(stdinNotifier_, &QSocketNotifier::activated, this, &LockdownHelperController::onStdinReadable);

        if (gSignalPipe[1] >= 0) {
            signalNotifier_ = new QSocketNotifier(gSignalPipe[1], QSocketNotifier::Read, this);
            connect(signalNotifier_, &QSocketNotifier::activated, this, [this] {
                char buf[16];
                const ssize_t ignored = ::read(gSignalPipe[1], buf, sizeof(buf));
                Q_UNUSED(ignored);
                shutdownCleanly();
            });
        }

        connect(&keyboard_, &seb::lockdown::KeyboardBlocker::altTabStepped, this, [](bool backwards) {
            writeJsonLine({
                {QStringLiteral("event"), QStringLiteral("alt_tab_step")},
                {QStringLiteral("backwards"), backwards},
            });
        });
        connect(&keyboard_, &seb::lockdown::KeyboardBlocker::altTabCommitted, this, [] {
            writeJsonLine({{QStringLiteral("event"), QStringLiteral("alt_tab_commit")}});
        });
        connect(&keyboard_, &seb::lockdown::KeyboardBlocker::altTabCancelled, this, [] {
            writeJsonLine({{QStringLiteral("event"), QStringLiteral("alt_tab_cancel")}});
        });

        writeJsonLine({
            {QStringLiteral("event"), QStringLiteral("ready")},
            {QStringLiteral("pid"), static_cast<qint64>(::getpid())},
            {QStringLiteral("euid"), static_cast<qint64>(::geteuid())},
        });
    }

    ~LockdownHelperController() override
    {
        keyboard_.unlock();
    }

private:
    void shutdownCleanly()
    {
        watchdogTimer_.stop();
        monitorTimer_.stop();
        if (locked_ || keyboard_.isLocked()) {
            keyboard_.unlock();
            locked_ = false;
            writeJsonLine({{QStringLiteral("event"), QStringLiteral("unlocked")}});
        }
        QCoreApplication::quit();
    }

    void onStdinReadable()
    {
        char chunk[4096];
        while (true) {
            const ssize_t bytes = ::read(STDIN_FILENO, chunk, sizeof(chunk));
            if (bytes > 0) {
                inputBuffer_.append(chunk, static_cast<qsizetype>(bytes));
            } else if (bytes == 0) {
                // Parent closed pipe (normal exit or crash) -> fail-safe immediate unlock!
                shutdownCleanly();
                return;
            } else {
                if (errno == EAGAIN || errno == EWOULDBLOCK) {
                    break;
                }
                shutdownCleanly();
                return;
            }
        }

        while (true) {
            const qsizetype newline = inputBuffer_.indexOf('\n');
            if (newline < 0) {
                break;
            }
            const QByteArray line = inputBuffer_.left(newline).trimmed();
            inputBuffer_.remove(0, newline + 1);
            if (!line.isEmpty()) {
                handleCommandLine(line);
            }
        }
    }

    void handleCommandLine(const QByteArray &line)
    {
        QJsonParseError parseError;
        const QJsonDocument doc = QJsonDocument::fromJson(line, &parseError);
        if (!doc.isObject()) {
            return;
        }

        const QJsonObject obj = doc.object();
        const QString cmd = obj.value(QStringLiteral("cmd")).toString();
        if (obj.contains(QStringLiteral("sebPid"))) {
            sebPid_ = obj.value(QStringLiteral("sebPid")).toInteger(sebPid_);
        }

        if (locked_) {
            watchdogTimer_.start();
        }

        if (cmd == QStringLiteral("ping")) {
            const QStringList found = locked_ ? scanProhibitedProcesses(blacklist_, sebPid_) : QStringList{};
            writeJsonLine({
                {QStringLiteral("event"), QStringLiteral("pong")},
                {QStringLiteral("locked"), locked_},
                {QStringLiteral("grabbedKeyboards"), keyboard_.grabbedDeviceCount()},
                {QStringLiteral("prohibited"), seb::lockdown::toJsonArray(found)},
            });
            return;
        }

        if (cmd == QStringLiteral("scan_processes")) {
            const QStringList list = seb::lockdown::fromJsonArray(obj.value(QStringLiteral("blacklist")).toArray());
            const QStringList found = scanProhibitedProcesses(list, sebPid_);
            writeJsonLine({
                {QStringLiteral("event"), QStringLiteral("scan_result")},
                {QStringLiteral("prohibited"), seb::lockdown::toJsonArray(found)},
            });
            return;
        }

        if (cmd == QStringLiteral("lock")) {
            blacklist_ = seb::lockdown::fromJsonArray(obj.value(QStringLiteral("blacklist")).toArray());
            const auto policy = seb::lockdown::KeyboardLockPolicy::fromJson(
                obj.value(QStringLiteral("keyboard")).toObject());
            QString kbError;
            const bool kbLocked = keyboard_.lock(policy, &kbError);
            locked_ = true;
            watchdogTimer_.start();
            monitorTimer_.start();
            writeJsonLine({
                {QStringLiteral("event"), QStringLiteral("locked")},
                {QStringLiteral("keyboardLocked"), kbLocked},
                {QStringLiteral("grabbedKeyboards"), keyboard_.grabbedDeviceCount()},
                {QStringLiteral("warning"), kbError},
            });
            return;
        }

        if (cmd == QStringLiteral("update_policy")) {
            if (obj.contains(QStringLiteral("blacklist"))) {
                blacklist_ = seb::lockdown::fromJsonArray(obj.value(QStringLiteral("blacklist")).toArray());
            }
            if (obj.contains(QStringLiteral("keyboard"))) {
                keyboard_.updatePolicy(seb::lockdown::KeyboardLockPolicy::fromJson(
                    obj.value(QStringLiteral("keyboard")).toObject()));
            }
            writeJsonLine({
                {QStringLiteral("event"), QStringLiteral("policy_updated")},
            });
            return;
        }

        if (cmd == QStringLiteral("unlock")) {
            shutdownCleanly();
        }
    }

    seb::lockdown::KeyboardBlocker keyboard_;
    QSocketNotifier *stdinNotifier_ = nullptr;
    QSocketNotifier *signalNotifier_ = nullptr;
    QTimer watchdogTimer_;
    QTimer monitorTimer_;
    QByteArray inputBuffer_;
    QStringList blacklist_;
    qint64 sebPid_ = 0;
    bool locked_ = false;
};

}  // namespace

int main(int argc, char *argv[])
{
    const pid_t parentBeforePrctl = ::getppid();
    ::prctl(PR_SET_PDEATHSIG, SIGTERM);
    if (::getppid() != parentBeforePrctl) {
        return 0;
    }

    if (::socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0, gSignalPipe) == 0) {
        std::signal(SIGTERM, posixSignalHandler);
        std::signal(SIGINT, posixSignalHandler);
        std::signal(SIGHUP, posixSignalHandler);
        std::signal(SIGPIPE, SIG_IGN);
    }

    QCoreApplication app(argc, argv);
    LockdownHelperController controller;
    const int exitCode = app.exec();

    if (gSignalPipe[0] >= 0) ::close(gSignalPipe[0]);
    if (gSignalPipe[1] >= 0) ::close(gSignalPipe[1]);
    return exitCode;
}
