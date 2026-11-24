#include "screen_proctoring_service.h"

#include <QApplication>
#include <QBuffer>
#include <QCoreApplication>
#include <QDateTime>
#include <QEvent>
#include <QImage>
#include <QImageWriter>
#include <QJsonDocument>
#include <QJsonObject>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QPixmap>
#include <QUuid>
#include <QWidget>

#include <algorithm>
#include <utility>

namespace seb::proctoring {

namespace {

constexpr int kMinIntervalFloorMs = 250;
constexpr int kDefaultMinIntervalMs = 1000;
constexpr int kDefaultMaxIntervalMs = 5000;
constexpr int kDefaultCacheSize = 500;
constexpr qsizetype kMaxQueuedBytes = 128 * 1024 * 1024;
constexpr int kMaxMetadataBytes = 4000;
constexpr int kNetworkTimeoutMs = 15000;
constexpr int kBaseRetryDelayMs = 2000;
constexpr int kMaxRetryDelayMs = 30000;

QByteArray formEncode(const QList<QPair<QString, QString>> &values)
{
    QByteArray body;
    for (const auto &pair : values) {
        if (!body.isEmpty()) {
            body += '&';
        }
        body += QUrl::toPercentEncoding(pair.first) + '=' + QUrl::toPercentEncoding(pair.second);
    }
    return body;
}

QImage quantizeGrayscaleLevels(const QImage &source, int levels)
{
    QImage gray = source.convertToFormat(QImage::Format_Grayscale8);
    if (gray.isNull() || levels < 2 || levels > 256) {
        return gray;
    }

    QImage indexed(gray.size(), QImage::Format_Indexed8);
    QList<QRgb> colorTable;
    colorTable.reserve(levels);
    for (int i = 0; i < levels; ++i) {
        const int intensity = (i * 255) / (levels - 1);
        colorTable.append(qRgb(intensity, intensity, intensity));
    }
    indexed.setColorTable(colorTable);

    const int width = gray.width();
    const int height = gray.height();
    for (int y = 0; y < height; ++y) {
        const uchar *src = gray.constScanLine(y);
        uchar *dst = indexed.scanLine(y);
        for (int x = 0; x < width; ++x) {
            dst[x] = static_cast<uchar>((static_cast<int>(src[x]) * (levels - 1) + 127) / 255);
        }
    }
    return indexed;
}

QByteArray headerValueCaseInsensitive(const QNetworkReply *reply, const QByteArray &name)
{
    if (!reply) {
        return {};
    }
    for (const auto &pair : reply->rawHeaderPairs()) {
        if (pair.first.compare(name, Qt::CaseInsensitive) == 0) {
            return pair.second.trimmed();
        }
    }
    return {};
}

bool supportsImageFormatWrite(const QByteArray &format)
{
    const auto supported = QImageWriter::supportedImageFormats();
    for (const QByteArray &candidate : supported) {
        if (candidate.compare(format, Qt::CaseInsensitive) == 0) {
            return true;
        }
    }
    return false;
}

QString truncateToLength(const QString &value, int maxChars)
{
    if (maxChars <= 0) {
        return {};
    }
    if (value.size() <= maxChars) {
        return value;
    }
    if (maxChars <= 3) {
        return value.left(maxChars);
    }
    return value.left(maxChars - 3) + QStringLiteral("...");
}

}  // namespace

ScreenProctoringService::ScreenProctoringService(
    WindowProvider windowProvider,
    ContextProvider contextProvider,
    QObject *parent)
    : QObject(parent)
    , windowProvider_(std::move(windowProvider))
    , contextProvider_(std::move(contextProvider))
{
    intervalTimer_.setSingleShot(false);
    connect(&intervalTimer_, &QTimer::timeout, this, [this] {
        captureWindowFrame(QStringLiteral("Interval"));
    });

    deferredActionTimer_.setSingleShot(true);
    connect(&deferredActionTimer_, &QTimer::timeout, this, [this] {
        const QString action = pendingActionDescription_.isEmpty()
            ? QStringLiteral("User interaction")
            : std::exchange(pendingActionDescription_, QString());
        captureWindowFrame(action);
    });

    retryTimer_.setSingleShot(true);
    connect(&retryTimer_, &QTimer::timeout, this, [this] {
        flushQueue();
    });
}

ScreenProctoringService::~ScreenProctoringService()
{
    stop(false);
}

bool ScreenProctoringService::start(
    const seb::settings::proctoring::ScreenProctoringSettings &settings,
    const QString &sessionId)
{
    stop(false);

    QUrl baseUrl;
    if (!normalizeServiceUrl(settings.ServiceUrl, baseUrl)) {
        const QString msg = QStringLiteral("Screen Proctoring could not start: invalid service URL.");
        emit logMessage(msg, QStringLiteral("ERROR"));
        setState(State::Error, msg);
        return false;
    }

    settings_ = settings;
    if (settings_.IntervalMinimum <= 0) {
        settings_.IntervalMinimum = kDefaultMinIntervalMs;
    }
    settings_.IntervalMinimum = qMax(kMinIntervalFloorMs, settings_.IntervalMinimum);
    if (settings_.IntervalMaximum <= 0) {
        settings_.IntervalMaximum = kDefaultMaxIntervalMs;
    }
    settings_.IntervalMaximum = qMax(settings_.IntervalMinimum, settings_.IntervalMaximum);
    if (settings_.CacheSize <= 0) {
        settings_.CacheSize = kDefaultCacheSize;
    }

    serviceBaseUrl_ = baseUrl;
    sessionId_ = sessionId.trimmed();
    oauthToken_.clear();
    sessionOpenedOnServer_ = false;
    running_ = true;
    retryAttempts_ = 0;
    sentCount_ = 0;
    queuedBytes_ = 0;
    queue_.clear();
    pendingActionDescription_.clear();
    ++generation_;

    if (qApp && !eventFilterInstalled_) {
        qApp->installEventFilter(this);
        eventFilterInstalled_ = true;
    }

    setState(State::Connecting, QStringLiteral("Screen Proctoring (Window only): Connecting..."));
    emit logMessage(QStringLiteral("Starting window-only Screen Proctoring service"), QStringLiteral("INFO"));

    const quint64 currentGen = generation_;
    requestOAuthToken([this, currentGen](bool tokenOk) {
        if (!running_ || currentGen != generation_) {
            return;
        }
        if (!tokenOk) {
            const QString msg = QStringLiteral("Screen Proctoring: OAuth authentication failed (retrying)");
            emit logMessage(msg, QStringLiteral("WARNING"));
            setState(State::Error, msg);
            scheduleNextCapture();
            scheduleRetry();
            return;
        }

        if (!sessionId_.isEmpty()) {
            sessionOpenedOnServer_ = true;
            setState(State::Active, QStringLiteral("Screen Proctoring (Window only): Active"));
            scheduleNextCapture();
            captureWindowFrame(QStringLiteral("Session started"));
            flushQueue();
            return;
        }

        openRemoteSession([this, currentGen](bool sessionOk) {
            if (!running_ || currentGen != generation_) {
                return;
            }
            if (!sessionOk) {
                const QString msg = QStringLiteral("Screen Proctoring: Session handshake failed (retrying)");
                emit logMessage(msg, QStringLiteral("WARNING"));
                setState(State::Error, msg);
                scheduleNextCapture();
                scheduleRetry();
                return;
            }

            sessionOpenedOnServer_ = true;
            setState(State::Active, QStringLiteral("Screen Proctoring (Window only): Active"));
            scheduleNextCapture();
            captureWindowFrame(QStringLiteral("Session started"));
            flushQueue();
        });
    });

    return true;
}

void ScreenProctoringService::stop(bool notifyServer)
{
    if (!running_ && activeReplies_.isEmpty() && !eventFilterInstalled_) {
        return;
    }

    const bool shouldNotifyClose = notifyServer && running_ && sessionOpenedOnServer_ &&
        !sessionId_.isEmpty() && !oauthToken_.isEmpty() && serviceBaseUrl_.isValid();

    running_ = false;
    ++generation_;

    intervalTimer_.stop();
    deferredActionTimer_.stop();
    retryTimer_.stop();

    if (qApp && eventFilterInstalled_) {
        qApp->removeEventFilter(this);
        eventFilterInstalled_ = false;
    }

    cancelNetworkActivity();

    if (shouldNotifyClose) {
        closeRemoteSession();
    }

    queue_.clear();
    queuedBytes_ = 0;
    pendingActionDescription_.clear();
    sessionOpenedOnServer_ = false;
    oauthToken_.clear();
    sessionId_.clear();

    emit queueChanged(0, sentCount_);
    setState(State::Inactive, QStringLiteral("Screen Proctoring: Inactive"));
}

bool ScreenProctoringService::isActive() const
{
    return running_ && (state_ == State::Active || state_ == State::Connecting);
}

void ScreenProctoringService::notifyUserAction(const QString &actionDescription)
{
    if (!running_) {
        return;
    }

    const qint64 elapsed = lastCaptureElapsed_.isValid() ? lastCaptureElapsed_.elapsed() : settings_.IntervalMinimum;
    if (elapsed >= settings_.IntervalMinimum) {
        deferredActionTimer_.stop();
        pendingActionDescription_.clear();
        captureWindowFrame(actionDescription);
    } else {
        pendingActionDescription_ = actionDescription;
        if (!deferredActionTimer_.isActive()) {
            const int remaining = qMax(50, settings_.IntervalMinimum - static_cast<int>(elapsed));
            deferredActionTimer_.start(remaining);
        }
    }
}

bool ScreenProctoringService::eventFilter(QObject *watched, QEvent *event)
{
    if (!running_ || !event) {
        return false;
    }

    const QEvent::Type type = event->type();
    if (type != QEvent::MouseButtonPress && type != QEvent::KeyPress) {
        return false;
    }

    QWidget *mainWindow = windowProvider_ ? windowProvider_() : nullptr;
    auto *targetWidget = qobject_cast<QWidget *>(watched);
    if (!mainWindow || !targetWidget) {
        return false;
    }

    // Scope events strictly to the Safe Exam Browser window hierarchy.
    if (targetWidget != mainWindow && !mainWindow->isAncestorOf(targetWidget)) {
        return false;
    }

    if (type == QEvent::MouseButtonPress) {
        auto *mouse = static_cast<QMouseEvent *>(event);
        const QPoint localPos = mainWindow->mapFromGlobal(mouse->globalPosition().toPoint());
        const QString buttonName = mouse->button() == Qt::RightButton
            ? QStringLiteral("Right")
            : (mouse->button() == Qt::MiddleButton ? QStringLiteral("Middle") : QStringLiteral("Left"));
        notifyUserAction(QStringLiteral("%1 mouse button pressed at (%2/%3).")
                             .arg(buttonName)
                             .arg(localPos.x())
                             .arg(localPos.y()));
    } else if (type == QEvent::KeyPress) {
        auto *key = static_cast<QKeyEvent *>(event);
        if (!key->isAutoRepeat()) {
            // Never record raw typed characters or passwords.
            notifyUserAction(QStringLiteral("Key pressed."));
        }
    }

    return false;
}

void ScreenProctoringService::setState(State state, const QString &statusText)
{
    if (state_ == state && statusText_ == statusText) {
        return;
    }
    state_ = state;
    statusText_ = statusText;
    emit stateChanged(state_, statusText_);
}

bool ScreenProctoringService::normalizeServiceUrl(const QString &rawUrl, QUrl &normalizedOut) const
{
    const QString trimmed = rawUrl.trimmed();
    if (trimmed.isEmpty()) {
        return false;
    }

    QUrl url = QUrl::fromUserInput(trimmed);
    if (!url.isValid() || url.host().isEmpty()) {
        return false;
    }
    const QString scheme = url.scheme().toLower();
    if (scheme != QStringLiteral("https") && scheme != QStringLiteral("http")) {
        return false;
    }
    url.setQuery(QString());
    url.setFragment(QString());
    QString path = url.path();
    while (path.endsWith(QLatin1Char('/'))) {
        path.chop(1);
    }
    url.setPath(path);
    normalizedOut = url;
    return true;
}

QUrl ScreenProctoringService::buildEndpointUrl(const QString &relativePath) const
{
    QUrl url = serviceBaseUrl_;
    QString path = url.path();
    if (!relativePath.startsWith(QLatin1Char('/'))) {
        path += QLatin1Char('/');
    }
    path += relativePath;
    url.setPath(path);
    return url;
}

void ScreenProctoringService::requestOAuthToken(std::function<void(bool)> done)
{
    if (done) {
        tokenWaiters_.append(std::move(done));
    }
    if (tokenInFlight_) {
        return;
    }

    if (settings_.ClientId.trimmed().isEmpty() && settings_.ClientSecret.trimmed().isEmpty()) {
        // Allow connecting to a test/local SPS endpoint that does not enforce OAuth2.
        const auto waiters = std::exchange(tokenWaiters_, {});
        for (const auto &waiter : waiters) {
            if (waiter) {
                waiter(true);
            }
        }
        return;
    }

    tokenInFlight_ = true;
    const quint64 currentGen = generation_;

    QNetworkRequest request(buildEndpointUrl(QStringLiteral("/oauth/token")));
    request.setTransferTimeout(kNetworkTimeoutMs);
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::ManualRedirectPolicy);
    request.setAttribute(QNetworkRequest::CookieLoadControlAttribute, QNetworkRequest::Manual);
    request.setAttribute(QNetworkRequest::CookieSaveControlAttribute, QNetworkRequest::Manual);
    request.setHeader(QNetworkRequest::ContentTypeHeader, QByteArrayLiteral("application/x-www-form-urlencoded"));
    request.setRawHeader("Accept", "application/json");
    const QByteArray credentials =
        (settings_.ClientId + QLatin1Char(':') + settings_.ClientSecret).toUtf8().toBase64();
    request.setRawHeader("Authorization", "Basic " + credentials);

    auto *reply = network_.post(
        request,
        QByteArrayLiteral("grant_type=client_credentials&scope=read%20write"));
    activeReplies_.insert(reply);

    connect(reply, &QNetworkReply::finished, this, [this, reply, currentGen] {
        activeReplies_.remove(reply);
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const QByteArray body = reply->readAll();
        const bool transportOk = reply->error() == QNetworkReply::NoError && status >= 200 && status < 300;
        reply->deleteLater();

        if (currentGen != generation_) {
            return;
        }

        tokenInFlight_ = false;
        bool ok = false;
        if (transportOk) {
            const QJsonDocument doc = QJsonDocument::fromJson(body);
            const QString token = doc.isObject()
                ? doc.object().value(QStringLiteral("access_token")).toString().trimmed()
                : QString();
            if (!token.isEmpty()) {
                oauthToken_ = token;
                ok = true;
            }
        }

        const auto waiters = std::exchange(tokenWaiters_, {});
        for (const auto &waiter : waiters) {
            if (currentGen != generation_) {
                break;
            }
            if (waiter) {
                waiter(ok);
            }
        }
    });
}

void ScreenProctoringService::openRemoteSession(std::function<void(bool)> done)
{
    const quint64 currentGen = generation_;

    QNetworkRequest request(buildEndpointUrl(QStringLiteral("/seb-api/v1/session")));
    request.setTransferTimeout(kNetworkTimeoutMs);
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::ManualRedirectPolicy);
    request.setAttribute(QNetworkRequest::CookieLoadControlAttribute, QNetworkRequest::Manual);
    request.setAttribute(QNetworkRequest::CookieSaveControlAttribute, QNetworkRequest::Manual);
    request.setHeader(QNetworkRequest::ContentTypeHeader, QByteArrayLiteral("application/x-www-form-urlencoded"));
    request.setRawHeader("Accept", "application/json, */*");
    if (!oauthToken_.isEmpty()) {
        request.setRawHeader("Authorization", "Bearer " + oauthToken_.toUtf8());
    }

    QList<QPair<QString, QString>> params;
    if (!settings_.GroupId.trimmed().isEmpty()) {
        params.append({QStringLiteral("groupId"), settings_.GroupId.trimmed()});
    }
    const QByteArray payload = formEncode(params);

    auto *reply = network_.post(request, payload);
    activeReplies_.insert(reply);

    connect(reply, &QNetworkReply::finished, this, [this, reply, currentGen, done = std::move(done)] {
        activeReplies_.remove(reply);
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const QByteArray body = reply->readAll();
        const bool ok = reply->error() == QNetworkReply::NoError && status >= 200 && status < 300;
        const QByteArray headerSessionId = !headerValueCaseInsensitive(reply, "SEB_SPS_SESSION_ID").isEmpty()
            ? headerValueCaseInsensitive(reply, "SEB_SPS_SESSION_ID")
            : headerValueCaseInsensitive(reply, "SEB-Session-Id");
        reply->deleteLater();

        if (currentGen != generation_) {
            return;
        }

        if (ok) {
            QString resolvedId = QString::fromUtf8(headerSessionId).trimmed();
            if (resolvedId.isEmpty()) {
                const QJsonDocument doc = QJsonDocument::fromJson(body);
                if (doc.isObject()) {
                    const QJsonObject obj = doc.object();
                    resolvedId = obj.value(QStringLiteral("sessionId")).toString().trimmed();
                    if (resolvedId.isEmpty()) {
                        resolvedId = obj.value(QStringLiteral("uuid")).toString().trimmed();
                    }
                }
            }
            if (resolvedId.isEmpty()) {
                resolvedId = QUuid::createUuid().toString(QUuid::WithoutBraces);
            }
            sessionId_ = resolvedId;
        }

        if (done) {
            done(ok);
        }
    });
}

void ScreenProctoringService::closeRemoteSession()
{
    if (sessionId_.isEmpty() || !serviceBaseUrl_.isValid()) {
        return;
    }

    const QString encodedSession = QString::fromLatin1(QUrl::toPercentEncoding(sessionId_));
    QNetworkRequest request(buildEndpointUrl(QStringLiteral("/seb-api/v1/session/%1").arg(encodedSession)));
    request.setTransferTimeout(3000);
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::ManualRedirectPolicy);
    request.setAttribute(QNetworkRequest::CookieLoadControlAttribute, QNetworkRequest::Manual);
    request.setAttribute(QNetworkRequest::CookieSaveControlAttribute, QNetworkRequest::Manual);
    if (!oauthToken_.isEmpty()) {
        request.setRawHeader("Authorization", "Bearer " + oauthToken_.toUtf8());
    }

    auto *reply = network_.deleteResource(request);
    connect(reply, &QNetworkReply::finished, reply, &QObject::deleteLater);
}

void ScreenProctoringService::cancelNetworkActivity()
{
    tokenInFlight_ = false;
    uploadInFlight_ = false;
    tokenWaiters_.clear();

    const auto replies = activeReplies_;
    activeReplies_.clear();
    for (QNetworkReply *reply : replies) {
        if (!reply) {
            continue;
        }
        reply->disconnect(this);
        reply->abort();
        reply->deleteLater();
    }
}

void ScreenProctoringService::scheduleNextCapture()
{
    if (!running_) {
        return;
    }
    if (!intervalTimer_.isActive()) {
        intervalTimer_.start(settings_.IntervalMaximum);
    }
}

void ScreenProctoringService::captureWindowFrame(const QString &triggerAction)
{
    if (!running_) {
        return;
    }

    QWidget *targetWindow = windowProvider_ ? windowProvider_() : nullptr;
    if (!targetWindow || !targetWindow->isVisible() || targetWindow->size().isEmpty()) {
        return;
    }

    // Capture ONLY the Safe Exam Browser window widget tree (never the full OS screen
    // or other desktop applications). On Linux/Wayland this also works cleanly without
    // triggering xdg-desktop-portal full-screen capture prompts.
    const QPixmap pixmap = targetWindow->grab();
    if (pixmap.isNull()) {
        return;
    }

    const QImage processed = processCapturedImage(pixmap.toImage());
    if (processed.isNull()) {
        return;
    }

    QString formatHeader;
    const QByteArray encoded = encodeImage(processed, formatHeader);
    if (encoded.isEmpty()) {
        return;
    }

    CaptureFrame frame;
    frame.timestampMs = QDateTime::currentMSecsSinceEpoch();
    frame.imageFormat = formatHeader;
    frame.imageData = encoded;
    frame.metadataJson = buildMetadataJson(triggerAction);

    lastCaptureElapsed_.restart();
    enqueueFrame(std::move(frame));
    flushQueue();
}

QImage ScreenProctoringService::processCapturedImage(const QImage &source) const
{
    if (source.isNull()) {
        return {};
    }

    QImage working = source;
    if (settings_.ImageDownscaling > 1.0) {
        const int targetWidth = qMax(16, static_cast<int>(working.width() / settings_.ImageDownscaling));
        const int targetHeight = qMax(16, static_cast<int>(working.height() / settings_.ImageDownscaling));
        working = working.scaled(targetWidth, targetHeight, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    }

    using Quantization = seb::settings::proctoring::ImageQuantization;
    switch (settings_.ImageQuantization) {
    case Quantization::BlackAndWhite1bpp:
        return working.convertToFormat(QImage::Format_Mono);
    case Quantization::Grayscale2bpp:
        return quantizeGrayscaleLevels(working, 4);
    case Quantization::Grayscale4bpp:
        return quantizeGrayscaleLevels(working, 16);
    case Quantization::Grayscale8bpp:
        return working.convertToFormat(QImage::Format_Grayscale8);
    case Quantization::Color8bpp:
        return working.convertToFormat(QImage::Format_Indexed8);
    case Quantization::Color16bpp:
        return working.convertToFormat(QImage::Format_RGB16);
    case Quantization::Color24bpp:
        return working.convertToFormat(QImage::Format_RGB888);
    }

    return working;
}

QByteArray ScreenProctoringService::encodeImage(const QImage &image, QString &formatOut) const
{
    using Format = seb::settings::proctoring::ImageFormat;
    const char *qtFormat = "PNG";
    int quality = -1;
    formatOut = QStringLiteral("png");

    switch (settings_.ImageFormat) {
    case Format::Jpg:
        qtFormat = "JPG";
        quality = 75;
        formatOut = QStringLiteral("jpg");
        break;
    case Format::Png:
        qtFormat = "PNG";
        formatOut = QStringLiteral("png");
        break;
    case Format::Bmp:
        qtFormat = "BMP";
        formatOut = QStringLiteral("bmp");
        break;
    case Format::Gif:
        if (supportsImageFormatWrite("gif")) {
            qtFormat = "GIF";
            formatOut = QStringLiteral("gif");
        } else {
            qtFormat = "PNG";
            formatOut = QStringLiteral("png");
        }
        break;
    }

    QImage writable = image;
    if (qstrcmp(qtFormat, "JPG") == 0) {
        if (writable.format() == QImage::Format_Mono ||
            writable.format() == QImage::Format_MonoLSB ||
            (writable.format() == QImage::Format_Indexed8 && writable.isGrayscale())) {
            writable = writable.convertToFormat(QImage::Format_Grayscale8);
        } else if (writable.format() != QImage::Format_Grayscale8 &&
                   writable.format() != QImage::Format_RGB888) {
            writable = writable.convertToFormat(QImage::Format_RGB888);
        }
    }

    QByteArray output;
    QBuffer buffer(&output);
    if (!buffer.open(QIODevice::WriteOnly)) {
        return {};
    }
    if (!writable.save(&buffer, qtFormat, quality)) {
        output.clear();
        buffer.seek(0);
        if (!writable.save(&buffer, "PNG")) {
            return {};
        }
        formatOut = QStringLiteral("png");
    }
    return output;
}

QByteArray ScreenProctoringService::buildMetadataJson(const QString &triggerAction) const
{
    const WindowMetadataContext ctx = contextProvider_ ? contextProvider_() : WindowMetadataContext{};
    const auto &meta = settings_.MetaData;

    QString executable;
    QString browserInfo;
    QString browserUrl;
    QString windowTitle;
    QString userAction = triggerAction.trimmed().isEmpty()
        ? QStringLiteral("Interval")
        : triggerAction.trimmed();

    if (meta.CaptureApplicationData) {
        executable = QStringLiteral("safe-exam-browser (%1)")
                         .arg(QCoreApplication::applicationName());
    }
    if (meta.CaptureBrowserData) {
        const QString prefix = ctx.isMainWindow
            ? QStringLiteral("Main Window: ")
            : QStringLiteral("Additional Window: ");
        browserInfo = prefix + (ctx.windowTitle.isEmpty() ? QStringLiteral("Safe Exam Browser") : ctx.windowTitle);
        browserUrl = ctx.browserUrl;
    }
    if (meta.CaptureWindowTitle) {
        windowTitle = ctx.windowTitle.isEmpty() ? QStringLiteral("Safe Exam Browser") : ctx.windowTitle;
    }

    auto serialize = [&]() -> QByteArray {
        QJsonObject obj;
        if (!executable.isEmpty()) {
            obj.insert(QStringLiteral("executable"), executable);
        }
        if (!browserInfo.isEmpty()) {
            obj.insert(QStringLiteral("screenProctoringMetadataBrowser"), browserInfo);
        }
        if (!browserUrl.isEmpty()) {
            obj.insert(QStringLiteral("screenProctoringMetadataURL"), browserUrl);
        }
        if (!userAction.isEmpty()) {
            obj.insert(QStringLiteral("screenProctoringMetadataUserAction"), userAction);
        }
        if (!windowTitle.isEmpty()) {
            obj.insert(QStringLiteral("screenProctoringMetadataWindowTitle"), windowTitle);
        }
        return QJsonDocument(obj).toJson(QJsonDocument::Compact);
    };

    QByteArray encoded = serialize();
    if (encoded.size() <= kMaxMetadataBytes) {
        return encoded;
    }

    // Keep metadata strictly valid JSON within SEB's 4000-byte limit.
    browserUrl = truncateToLength(browserUrl, 1024);
    browserInfo = truncateToLength(browserInfo, 512);
    windowTitle = truncateToLength(windowTitle, 512);
    userAction = truncateToLength(userAction, 256);
    executable = truncateToLength(executable, 256);
    encoded = serialize();
    if (encoded.size() <= kMaxMetadataBytes) {
        return encoded;
    }

    browserUrl = truncateToLength(browserUrl, 256);
    browserInfo = truncateToLength(browserInfo, 128);
    windowTitle = truncateToLength(windowTitle, 128);
    return serialize();
}

void ScreenProctoringService::enqueueFrame(CaptureFrame &&frame)
{
    const qsizetype frameSize = frame.imageData.size() + frame.metadataJson.size();
    while (!queue_.isEmpty() &&
           (queue_.size() >= settings_.CacheSize || queuedBytes_ + frameSize > kMaxQueuedBytes)) {
        const CaptureFrame dropped = queue_.dequeue();
        queuedBytes_ = qMax<qsizetype>(0, queuedBytes_ - dropped.imageData.size() - dropped.metadataJson.size());
    }

    queuedBytes_ += frameSize;
    queue_.enqueue(std::move(frame));
    emit queueChanged(static_cast<int>(queue_.size()), sentCount_);
}

void ScreenProctoringService::flushQueue()
{
    if (!running_ || uploadInFlight_ || queue_.isEmpty()) {
        return;
    }

    if (!sessionOpenedOnServer_) {
        if (tokenInFlight_ || retryTimer_.isActive()) {
            return;
        }
        const quint64 currentGen = generation_;
        requestOAuthToken([this, currentGen](bool tokenOk) {
            if (!running_ || currentGen != generation_) {
                return;
            }
            if (!tokenOk) {
                scheduleRetry();
                return;
            }
            if (!sessionId_.isEmpty()) {
                sessionOpenedOnServer_ = true;
                retryAttempts_ = 0;
                setState(State::Active, QStringLiteral("Screen Proctoring (Window only): Active"));
                flushQueue();
                return;
            }
            openRemoteSession([this, currentGen](bool sessionOk) {
                if (!running_ || currentGen != generation_) {
                    return;
                }
                if (!sessionOk) {
                    scheduleRetry();
                    return;
                }
                sessionOpenedOnServer_ = true;
                retryAttempts_ = 0;
                setState(State::Active, QStringLiteral("Screen Proctoring (Window only): Active"));
                flushQueue();
            });
        });
        return;
    }

    uploadInFlight_ = true;
    const CaptureFrame frame = queue_.head();
    uploadFrame(frame, false);
}

void ScreenProctoringService::uploadFrame(const CaptureFrame &frame, bool retriedAuth)
{
    const quint64 currentGen = generation_;
    const QString encodedSession = QString::fromLatin1(QUrl::toPercentEncoding(sessionId_));
    QNetworkRequest request(
        buildEndpointUrl(QStringLiteral("/seb-api/v1/session/%1/screenshot").arg(encodedSession)));
    request.setTransferTimeout(kNetworkTimeoutMs);
    request.setAttribute(QNetworkRequest::RedirectPolicyAttribute, QNetworkRequest::ManualRedirectPolicy);
    request.setAttribute(QNetworkRequest::CookieLoadControlAttribute, QNetworkRequest::Manual);
    request.setAttribute(QNetworkRequest::CookieSaveControlAttribute, QNetworkRequest::Manual);
    request.setHeader(QNetworkRequest::ContentTypeHeader, QByteArrayLiteral("application/octet-stream"));
    request.setRawHeader("Accept", "application/json, */*");
    if (!oauthToken_.isEmpty()) {
        request.setRawHeader("Authorization", "Bearer " + oauthToken_.toUtf8());
    }

    const QByteArray tsBytes = QByteArray::number(frame.timestampMs);
    const QByteArray formatBytes = frame.imageFormat.toUtf8();
    const QByteArray encodedMeta = QUrl::toPercentEncoding(QString::fromUtf8(frame.metadataJson));

    request.setRawHeader("timestamp", tsBytes);
    request.setRawHeader("SEB_SPS_TIMESTAMP", tsBytes);
    request.setRawHeader("imageFormat", formatBytes);
    request.setRawHeader("SEB_SPS_IMAGE_FORMAT", formatBytes);
    request.setRawHeader("metaData", encodedMeta);
    request.setRawHeader("SEB_SPS_METADATA", encodedMeta);

    auto *reply = network_.post(request, frame.imageData);
    activeReplies_.insert(reply);

    connect(reply, &QNetworkReply::finished, this, [this, reply, currentGen, frame, retriedAuth] {
        activeReplies_.remove(reply);
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const bool ok = reply->error() == QNetworkReply::NoError && status >= 200 && status < 300;
        reply->deleteLater();

        if (!running_ || currentGen != generation_) {
            return;
        }

        if (ok) {
            uploadInFlight_ = false;
            retryAttempts_ = 0;
            if (!queue_.isEmpty() && queue_.head().timestampMs == frame.timestampMs) {
                const CaptureFrame sent = queue_.dequeue();
                queuedBytes_ = qMax<qsizetype>(0, queuedBytes_ - sent.imageData.size() - sent.metadataJson.size());
            }
            ++sentCount_;
            setState(State::Active,
                     QStringLiteral("Screen Proctoring (Window only): Active (%1 sent, %2 queued)")
                         .arg(sentCount_)
                         .arg(queue_.size()));
            emit queueChanged(static_cast<int>(queue_.size()), sentCount_);
            if (!queue_.isEmpty()) {
                flushQueue();
            }
            return;
        }

        if (status == 401 && !retriedAuth) {
            requestOAuthToken([this, currentGen, frame](bool refreshed) {
                if (!running_ || currentGen != generation_) {
                    return;
                }
                if (refreshed) {
                    uploadFrame(frame, true);
                } else {
                    uploadInFlight_ = false;
                    setState(State::Error, QStringLiteral("Screen Proctoring: Authentication expired (retrying)"));
                    scheduleRetry();
                }
            });
            return;
        }

        uploadInFlight_ = false;
        setState(State::Error,
                 QStringLiteral("Screen Proctoring: Upload failed (HTTP %1, %2 queued — retrying)")
                     .arg(status)
                     .arg(queue_.size()));
        scheduleRetry();
    });
}

void ScreenProctoringService::scheduleRetry()
{
    if (!running_ || retryTimer_.isActive()) {
        return;
    }
    const int shift = qMin(retryAttempts_, 4);
    const int delay = qMin(kMaxRetryDelayMs, kBaseRetryDelayMs * (1 << shift));
    ++retryAttempts_;
    retryTimer_.start(delay);
}

}  // namespace seb::proctoring
