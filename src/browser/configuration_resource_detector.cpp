#include "configuration_resource_detector.h"

namespace seb::browser {

bool ConfigurationResourceDetector::isConfigurationResource(
    const QUrl &url,
    const QString &suggestedFileName,
    const QString &mimeType)
{
    const QString decodedPath = url.path(QUrl::FullyDecoded);
    const QString mimeBase = mimeType.section(QLatin1Char(';'), 0, 0).trimmed();
    const bool isDownloadContext = !suggestedFileName.isEmpty() || !mimeBase.isEmpty();
    const bool schemeMatch = url.scheme().compare(QStringLiteral("seb"), Qt::CaseInsensitive) == 0 ||
                             url.scheme().compare(QStringLiteral("sebs"), Qt::CaseInsensitive) == 0;
    const bool fileNameMatch = suggestedFileName.endsWith(QStringLiteral(".seb"), Qt::CaseInsensitive);
    const bool pathMatch = decodedPath.endsWith(QStringLiteral(".seb"), Qt::CaseInsensitive) ||
                           (isDownloadContext &&
                            decodedPath.endsWith(QStringLiteral("/accessrule/seb/config.php"), Qt::CaseInsensitive));
    const bool mimeMatch = mimeBase.compare(QStringLiteral("application/seb"), Qt::CaseInsensitive) == 0 ||
                           mimeBase.compare(QStringLiteral("application/x-seb"), Qt::CaseInsensitive) == 0;
    const bool dataMatch = QUrl::fromPercentEncoding(url.toEncoded()).contains(
        QStringLiteral("application/seb"), Qt::CaseInsensitive);

    return schemeMatch || fileNameMatch || pathMatch || mimeMatch || dataMatch;
}

QUrl ConfigurationResourceDetector::downloadUrl(const QUrl &url)
{
    const QString encoded = url.toString(QUrl::FullyEncoded);
    const bool isDataConfiguration = encoded.contains(
        QStringLiteral("application/seb"),
        Qt::CaseInsensitive);
    const QString scheme = url.scheme();

    if (isDataConfiguration &&
        (scheme.compare(QStringLiteral("seb"), Qt::CaseInsensitive) == 0 ||
         scheme.compare(QStringLiteral("sebs"), Qt::CaseInsensitive) == 0)) {
        const qsizetype separator = encoded.indexOf(QStringLiteral("://"));
        if (separator >= 0) {
            return QUrl(
                QStringLiteral("data:") +
                encoded.mid(separator + 3));
        }
    }

    QUrl converted(url);
    if (scheme.compare(QStringLiteral("seb"), Qt::CaseInsensitive) == 0) {
        converted.setScheme(QStringLiteral("http"));
    } else if (scheme.compare(QStringLiteral("sebs"), Qt::CaseInsensitive) == 0) {
        converted.setScheme(QStringLiteral("https"));
    }
    return converted;
}

}  // namespace seb::browser
