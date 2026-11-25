#include "lockdown_client.h"

#include <QCoreApplication>
#include <QDebug>
#include <QEventLoop>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSet>
#include <QStandardPaths>

#include <unistd.h>

namespace seb::security {

namespace {

constexpr int kPolkitAuthTimeoutMs = 120000;
constexpr int kIpcCommandTimeoutMs = 5000;

}  // namespace

LockdownClient::LockdownClient(QObject *parent)
    : QObject(parent)
{
    heartbeatTimer_.setSingleShot(false);
    heartbeatTimer_.setInterval(seb::lockdown::kHeartbeatIntervalMs);
    connect(&heartbeatTimer_, &QTimer::timeout, this, [this] {
        if (!locked_ || process_.state() != QProcess::Running) {
            return;
        }
        sendJsonCommand({
            {QStringLiteral("cmd"), QStringLiteral("ping")},
            {QStringLiteral("sebPid"), static_cast<qint64>(::getpid())},
        });
    });

    connect(&process_, &QProcess::readyReadStandardOutput, this, &LockdownClient::handleStdoutReady);
    connect(&process_, &QProcess::finished, this, [this](int exitCode, QProcess::ExitStatus exitStatus) {
        const bool wasExpected = stopping_;
        const bool wasActive = armed_ || locked_;
        heartbeatTimer_.stop();
        armed_ = false;
        locked_ = false;
        keyboardLocked_ = false;

        if (!wasExpected && wasActive) {
            const QString reason = exitStatus == QProcess::CrashExit
                ? QStringLiteral("The privileged SEB lockdown helper crashed unexpectedly.")
                : QStringLiteral("The privileged SEB lockdown helper exited unexpectedly (code %1).").arg(exitCode);
            emit helperTerminatedUnexpectedly(reason);
        }
    });
}

LockdownClient::~LockdownClient()
{
    stop();
}

QString LockdownClient::resolveHelperPath() const
{
    const QString appDirCandidate =
        QCoreApplication::applicationDirPath() + QStringLiteral("/safe-exam-browser-lockdown");
    if (QFileInfo::exists(appDirCandidate) && QFileInfo(appDirCandidate).isExecutable()) {
        return appDirCandidate;
    }

    const QStringList systemCandidates{
        QStringLiteral("/usr/libexec/safe-exam-browser-lockdown"),
        QStringLiteral("/usr/lib/safe-exam-browser/safe-exam-browser-lockdown"),
        QStringLiteral("/usr/bin/safe-exam-browser-lockdown"),
    };
    for (const QString &candidate : systemCandidates) {
        if (QFileInfo::exists(candidate) && QFileInfo(candidate).isExecutable()) {
            return candidate;
        }
    }

    return QStandardPaths::findExecutable(QStringLiteral("safe-exam-browser-lockdown"));
}

bool LockdownClient::startHelper(QString *error)
{
    if (armed_ && process_.state() == QProcess::Running) {
        return true;
    }

    stop();

    const QString pkexecPath = QStandardPaths::findExecutable(QStringLiteral("pkexec"));
    if (pkexecPath.isEmpty()) {
        if (error) {
            *error = QStringLiteral("Polkit (pkexec) was not found. Administrator authentication is required for Secure Anti-Cheat Mode.");
        }
        return false;
    }

    const QString helperPath = resolveHelperPath();
    if (helperPath.isEmpty()) {
        if (error) {
            *error = QStringLiteral("The Safe Exam Browser lockdown helper binary (safe-exam-browser-lockdown) was not found.");
        }
        return false;
    }

    stopping_ = false;
    violationReported_ = false;
    stdoutBuffer_.clear();
    lastError_.clear();
    awaitingReady_ = true;

    QEventLoop loop;
    QTimer timeout;
    timeout.setSingleShot(true);
    bool timedOut = false;

    const auto readyConn = connect(this, &LockdownClient::internalReadyReceived, &loop, &QEventLoop::quit);
    const auto finishedConn = connect(&process_, &QProcess::finished, &loop, &QEventLoop::quit);
    const auto errorConn = connect(&process_, &QProcess::errorOccurred, &loop, [&loop](QProcess::ProcessError) {
        loop.quit();
    });
    connect(&timeout, &QTimer::timeout, &loop, [&loop, &timedOut] {
        timedOut = true;
        loop.quit();
    });

    process_.setProcessChannelMode(QProcess::SeparateChannels);
    process_.start(pkexecPath, {helperPath});
    if (process_.state() != QProcess::NotRunning && !armed_) {
        timeout.start(kPolkitAuthTimeoutMs);
        loop.exec();
    }

    disconnect(readyConn);
    disconnect(finishedConn);
    disconnect(errorConn);
    awaitingReady_ = false;

    if (armed_ && process_.state() == QProcess::Running) {
        return true;
    }

    const QString stderrText = QString::fromUtf8(process_.readAllStandardError()).trimmed();
    const int exitCode = process_.exitCode();
    stop();

    if (error) {
        if (timedOut) {
            *error = QStringLiteral("Administrator authentication timed out.");
        } else if (exitCode == 126 || exitCode == 127) {
            *error = QStringLiteral("Administrator authentication was cancelled or denied. Safe Exam Browser requires superuser lockdown in exam mode.");
        } else if (!stderrText.isEmpty()) {
            *error = QStringLiteral("Unable to start SEB lockdown helper: %1").arg(stderrText);
        } else {
            *error = QStringLiteral("Unable to start SEB lockdown helper.");
        }
    }
    return false;
}

bool LockdownClient::scanProhibitedProcesses(
    const QStringList &blacklist,
    QStringList &detectedOut,
    QString *error)
{
    detectedOut.clear();
    if (!armed_ || process_.state() != QProcess::Running) {
        if (error) {
            *error = QStringLiteral("SEB lockdown helper is not running.");
        }
        return false;
    }

    lastScanResult_.clear();
    awaitingScan_ = true;

    QEventLoop loop;
    QTimer timeout;
    timeout.setSingleShot(true);
    bool timedOut = false;
    bool gotResult = false;

    const auto scanConn = connect(this, &LockdownClient::internalScanReceived, &loop, [&loop, &gotResult] {
        gotResult = true;
        loop.quit();
    });
    const auto finishedConn = connect(&process_, &QProcess::finished, &loop, &QEventLoop::quit);
    connect(&timeout, &QTimer::timeout, &loop, [&loop, &timedOut] {
        timedOut = true;
        loop.quit();
    });

    if (!sendJsonCommand({
            {QStringLiteral("cmd"), QStringLiteral("scan_processes")},
            {QStringLiteral("sebPid"), static_cast<qint64>(::getpid())},
            {QStringLiteral("blacklist"), seb::lockdown::toJsonArray(blacklist)},
        })) {
        disconnect(scanConn);
        disconnect(finishedConn);
        awaitingScan_ = false;
        if (error) {
            *error = QStringLiteral("Failed to communicate with SEB lockdown helper.");
        }
        return false;
    }

    timeout.start(kIpcCommandTimeoutMs);
    loop.exec();

    disconnect(scanConn);
    disconnect(finishedConn);
    awaitingScan_ = false;

    if (!gotResult) {
        if (error) {
            *error = timedOut
                ? QStringLiteral("Timed out while scanning running processes.")
                : QStringLiteral("SEB lockdown helper disconnected while scanning processes.");
        }
        return false;
    }

    detectedOut = lastScanResult_;
    return true;
}

bool LockdownClient::activateLockdown(
    const seb::lockdown::KeyboardLockPolicy &policy,
    const QStringList &blacklist,
    QString *error)
{
    if (!armed_ || process_.state() != QProcess::Running) {
        if (error) {
            *error = QStringLiteral("SEB lockdown helper is not running.");
        }
        return false;
    }

    if (locked_) {
        updatePolicy(policy, blacklist);
        return true;
    }

    awaitingLock_ = true;
    violationReported_ = false;

    QEventLoop loop;
    QTimer timeout;
    timeout.setSingleShot(true);
    bool timedOut = false;
    bool gotLocked = false;

    const auto lockConn = connect(this, &LockdownClient::internalLockedReceived, &loop, [&loop, &gotLocked] {
        gotLocked = true;
        loop.quit();
    });
    const auto finishedConn = connect(&process_, &QProcess::finished, &loop, &QEventLoop::quit);
    connect(&timeout, &QTimer::timeout, &loop, [&loop, &timedOut] {
        timedOut = true;
        loop.quit();
    });

    if (!sendJsonCommand({
            {QStringLiteral("cmd"), QStringLiteral("lock")},
            {QStringLiteral("sebPid"), static_cast<qint64>(::getpid())},
            {QStringLiteral("keyboard"), policy.toJson()},
            {QStringLiteral("blacklist"), seb::lockdown::toJsonArray(blacklist)},
        })) {
        disconnect(lockConn);
        disconnect(finishedConn);
        awaitingLock_ = false;
        if (error) {
            *error = QStringLiteral("Failed to send lockdown command to helper.");
        }
        return false;
    }

    timeout.start(kIpcCommandTimeoutMs);
    loop.exec();

    disconnect(lockConn);
    disconnect(finishedConn);
    awaitingLock_ = false;

    if (!gotLocked || !locked_) {
        if (error) {
            *error = timedOut
                ? QStringLiteral("Timed out while activating SEB lockdown.")
                : QStringLiteral("Failed to activate SEB lockdown.");
        }
        return false;
    }

    heartbeatTimer_.start();
    return true;
}

void LockdownClient::updatePolicy(
    const seb::lockdown::KeyboardLockPolicy &policy,
    const QStringList &blacklist)
{
    if (!armed_ || process_.state() != QProcess::Running) {
        return;
    }

    sendJsonCommand({
        {QStringLiteral("cmd"), QStringLiteral("update_policy")},
        {QStringLiteral("sebPid"), static_cast<qint64>(::getpid())},
        {QStringLiteral("keyboard"), policy.toJson()},
        {QStringLiteral("blacklist"), seb::lockdown::toJsonArray(blacklist)},
    });
}

void LockdownClient::stop()
{
    stopping_ = true;
    heartbeatTimer_.stop();

    if (process_.state() == QProcess::Running) {
        sendJsonCommand({{QStringLiteral("cmd"), QStringLiteral("unlock")}});
        process_.closeWriteChannel();
        if (!process_.waitForFinished(1200)) {
            process_.terminate();
            if (!process_.waitForFinished(800)) {
                process_.kill();
                process_.waitForFinished(500);
            }
        }
    }

    armed_ = false;
    locked_ = false;
    keyboardLocked_ = false;
    stdoutBuffer_.clear();
}

seb::lockdown::KeyboardLockPolicy LockdownClient::keyboardPolicyFromSettings(
    const seb::settings::AppSettings &settings)
{
    seb::lockdown::KeyboardLockPolicy policy;
    // On Linux without an isolated Windows desktop, allowing OS-level Alt+Tab, Alt+F4,
    // Alt+Esc, Ctrl+Esc, PrintScreen, or Super/Meta would let the user escape to other
    // desktop applications. Always enforce OS-level blocking in strict lockdown mode.
    policy.allowAltTab = false;
    policy.allowAltF4 = false;
    policy.allowAltEsc = false;
    policy.allowCtrlEsc = false;
    policy.allowPrintScreen = false;
    policy.allowSystemKey = false;
    policy.allowActionCenterCombo = settings.UserInterface.ActionCenter.EnableActionCenter;
    return policy;
}

QStringList LockdownClient::buildProhibitedList(const seb::settings::AppSettings &settings)
{
    QStringList merged = seb::lockdown::defaultProhibitedProcesses();
    QSet<QString> seen;
    for (const QString &name : merged) {
        seen.insert(name.toLower());
    }

    for (const auto &rule : settings.Applications.Blacklist) {
        const QString fileName = QFileInfo(rule.ExecutableName.trimmed()).fileName();
        if (!fileName.isEmpty() && !seen.contains(fileName.toLower())) {
            seen.insert(fileName.toLower());
            merged.append(fileName);
        }
    }
    return merged;
}

bool LockdownClient::sendJsonCommand(const QJsonObject &command)
{
    if (process_.state() != QProcess::Running) {
        return false;
    }
    QByteArray line = QJsonDocument(command).toJson(QJsonDocument::Compact);
    line.append('\n');
    const qint64 written = process_.write(line);
    if (written != line.size()) {
        return false;
    }
    process_.waitForBytesWritten(500);
    return true;
}

void LockdownClient::handleStdoutReady()
{
    stdoutBuffer_.append(process_.readAllStandardOutput());
    while (true) {
        const qsizetype newline = stdoutBuffer_.indexOf('\n');
        if (newline < 0) {
            break;
        }
        const QByteArray line = stdoutBuffer_.left(newline).trimmed();
        stdoutBuffer_.remove(0, newline + 1);
        if (line.isEmpty()) {
            continue;
        }
        QJsonParseError parseError;
        const QJsonDocument doc = QJsonDocument::fromJson(line, &parseError);
        if (doc.isObject()) {
            handleEvent(doc.object());
        }
    }
}

void LockdownClient::handleEvent(const QJsonObject &event)
{
    const QString type = event.value(QStringLiteral("event")).toString();

    if (type == QStringLiteral("ready")) {
        armed_ = true;
        if (awaitingReady_) {
            emit internalReadyReceived();
        }
        return;
    }

    if (type == QStringLiteral("scan_result")) {
        lastScanResult_ = seb::lockdown::fromJsonArray(event.value(QStringLiteral("prohibited")).toArray());
        if (awaitingScan_) {
            emit internalScanReceived();
        }
        return;
    }

    if (type == QStringLiteral("locked")) {
        locked_ = true;
        keyboardLocked_ = event.value(QStringLiteral("keyboardLocked")).toBool(false);
        const QString warning = event.value(QStringLiteral("warning")).toString().trimmed();
        if (!warning.isEmpty()) {
            qWarning().noquote() << "[Lockdown]" << warning;
        } else {
            qDebug().noquote() << "[Lockdown] Kernel keyboard filter active on"
                               << event.value(QStringLiteral("grabbedKeyboards")).toInt()
                               << "keyboard device(s).";
        }
        if (awaitingLock_) {
            emit internalLockedReceived();
        }
        return;
    }

    if (type == QStringLiteral("unlocked")) {
        locked_ = false;
        keyboardLocked_ = false;
        return;
    }

    if (type == QStringLiteral("violation")) {
        if (violationReported_ || stopping_) {
            return;
        }
        violationReported_ = true;
        const QString reason = event.value(QStringLiteral("reason")).toString();
        const QStringList prohibited =
            seb::lockdown::fromJsonArray(event.value(QStringLiteral("prohibited")).toArray());
        emit violationDetected(
            reason.isEmpty() ? QStringLiteral("Prohibited application detected during exam.") : reason,
            prohibited);
        return;
    }

    if (type == QStringLiteral("pong")) {
        const QStringList prohibited =
            seb::lockdown::fromJsonArray(event.value(QStringLiteral("prohibited")).toArray());
        if (!prohibited.isEmpty() && !violationReported_ && !stopping_) {
            violationReported_ = true;
            emit violationDetected(
                QStringLiteral("Prohibited applications detected during exam: %1")
                    .arg(prohibited.join(QStringLiteral(", "))),
                prohibited);
        }
        return;
    }

    if (type == QStringLiteral("alt_tab_step")) {
        emit altTabStepped(event.value(QStringLiteral("backwards")).toBool(false));
        return;
    }

    if (type == QStringLiteral("alt_tab_commit")) {
        emit altTabCommitted();
        return;
    }

    if (type == QStringLiteral("alt_tab_cancel")) {
        emit altTabCancelled();
        return;
    }

    if (type == QStringLiteral("error")) {
        lastError_ = event.value(QStringLiteral("message")).toString();
        qWarning().noquote() << "[Lockdown]" << lastError_;
    }
}

}  // namespace seb::security
