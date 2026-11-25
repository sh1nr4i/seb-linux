#pragma once

#include "lockdown/lockdown_protocol.h"
#include "settings/AppSettings.h"

#include <QObject>
#include <QProcess>
#include <QString>
#include <QStringList>
#include <QTimer>

namespace seb::security {

class LockdownClient : public QObject
{
    Q_OBJECT

public:
    explicit LockdownClient(QObject *parent = nullptr);
    ~LockdownClient() override;

    bool startHelper(QString *error = nullptr);
    bool scanProhibitedProcesses(
        const QStringList &blacklist,
        QStringList &detectedOut,
        QString *error = nullptr);
    bool activateLockdown(
        const seb::lockdown::KeyboardLockPolicy &policy,
        const QStringList &blacklist,
        QString *error = nullptr);
    void updatePolicy(
        const seb::lockdown::KeyboardLockPolicy &policy,
        const QStringList &blacklist);
    void stop();

    bool isArmed() const { return armed_; }
    bool isLocked() const { return locked_; }
    bool isKeyboardLocked() const { return keyboardLocked_; }

    static seb::lockdown::KeyboardLockPolicy keyboardPolicyFromSettings(
        const seb::settings::AppSettings &settings);
    static QStringList buildProhibitedList(
        const seb::settings::AppSettings &settings);

signals:
    void violationDetected(const QString &reason, const QStringList &prohibited);
    void helperTerminatedUnexpectedly(const QString &reason);
    void altTabStepped(bool backwards);
    void altTabCommitted();
    void altTabCancelled();

private:
    QString resolveHelperPath() const;
    bool sendJsonCommand(const QJsonObject &command);
    void handleStdoutReady();
    void handleEvent(const QJsonObject &event);

    QProcess process_;
    QTimer heartbeatTimer_;
    QByteArray stdoutBuffer_;
    bool armed_ = false;
    bool locked_ = false;
    bool keyboardLocked_ = false;
    bool stopping_ = false;
    bool violationReported_ = false;

    bool awaitingReady_ = false;
    bool awaitingScan_ = false;
    bool awaitingLock_ = false;
    QStringList lastScanResult_;
    QString lastError_;

signals:
    void internalReadyReceived();
    void internalScanReceived();
    void internalLockedReceived();
};

}  // namespace seb::security
