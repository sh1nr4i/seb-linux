#pragma once

#include "settings/Proctoring/ScreenProctoringSettings.h"

#include <QByteArray>
#include <QElapsedTimer>
#include <QNetworkAccessManager>
#include <QObject>
#include <QPointer>
#include <QQueue>
#include <QSet>
#include <QString>
#include <QTimer>
#include <QUrl>

#include <functional>

QT_BEGIN_NAMESPACE
class QImage;
class QNetworkReply;
class QWidget;
QT_END_NAMESPACE

namespace seb::proctoring {

struct WindowMetadataContext
{
    QString windowTitle;
    QString browserUrl;
    bool isMainWindow = true;
};

class ScreenProctoringService : public QObject
{
    Q_OBJECT

public:
    enum class State
    {
        Inactive,
        Connecting,
        Active,
        Error
    };
    Q_ENUM(State)

    using WindowProvider = std::function<QWidget *()>;
    using ContextProvider = std::function<WindowMetadataContext()>;

    explicit ScreenProctoringService(
        WindowProvider windowProvider,
        ContextProvider contextProvider,
        QObject *parent = nullptr);
    ~ScreenProctoringService() override;

    bool start(
        const seb::settings::proctoring::ScreenProctoringSettings &settings,
        const QString &sessionId = {});
    void stop(bool notifyServer = true);

    bool isActive() const;
    State state() const { return state_; }
    QString statusText() const { return statusText_; }
    int queuedCount() const { return static_cast<int>(queue_.size()); }
    quint64 sentCount() const { return sentCount_; }

    void notifyUserAction(const QString &actionDescription);

signals:
    void stateChanged(seb::proctoring::ScreenProctoringService::State state, const QString &statusText);
    void queueChanged(int queuedCount, quint64 sentCount);
    void logMessage(const QString &message, const QString &severity);

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    struct CaptureFrame
    {
        qint64 timestampMs = 0;
        QString imageFormat;
        QByteArray imageData;
        QByteArray metadataJson;
    };

    void setState(State state, const QString &statusText);
    bool normalizeServiceUrl(const QString &rawUrl, QUrl &normalizedOut) const;
    QUrl buildEndpointUrl(const QString &relativePath) const;

    void requestOAuthToken(std::function<void(bool)> done);
    void openRemoteSession(std::function<void(bool)> done);
    void closeRemoteSession();
    void cancelNetworkActivity();

    void scheduleNextCapture();
    void captureWindowFrame(const QString &triggerAction);
    QImage processCapturedImage(const QImage &source) const;
    QByteArray encodeImage(const QImage &image, QString &formatOut) const;
    QByteArray buildMetadataJson(const QString &triggerAction) const;
    void enqueueFrame(CaptureFrame &&frame);

    void flushQueue();
    void uploadFrame(const CaptureFrame &frame, bool retriedAuth = false);
    void scheduleRetry();

    WindowProvider windowProvider_;
    ContextProvider contextProvider_;

    seb::settings::proctoring::ScreenProctoringSettings settings_;
    QUrl serviceBaseUrl_;
    QString sessionId_;
    QString oauthToken_;
    bool sessionOpenedOnServer_ = false;
    bool running_ = false;
    bool eventFilterInstalled_ = false;
    bool tokenInFlight_ = false;
    bool uploadInFlight_ = false;
    int retryAttempts_ = 0;
    quint64 generation_ = 0;
    quint64 sentCount_ = 0;
    qsizetype queuedBytes_ = 0;

    State state_ = State::Inactive;
    QString statusText_;

    QNetworkAccessManager network_;
    QSet<QNetworkReply *> activeReplies_;
    QList<std::function<void(bool)>> tokenWaiters_;
    QQueue<CaptureFrame> queue_;
    QTimer intervalTimer_;
    QTimer deferredActionTimer_;
    QTimer retryTimer_;
    QElapsedTimer lastCaptureElapsed_;
    QString pendingActionDescription_;
};

}  // namespace seb::proctoring
