#pragma once

#include <QJsonArray>
#include <QJsonObject>
#include <QString>
#include <QStringList>

namespace seb::lockdown {

inline constexpr const char *kVirtualKeyboardName = "SEB Virtual Keyboard";
inline constexpr int kHeartbeatIntervalMs = 2000;
inline constexpr int kWatchdogTimeoutMs = 6000;

struct KeyboardLockPolicy
{
    bool allowAltTab = false;
    bool allowAltF4 = false;
    bool allowAltEsc = false;
    bool allowCtrlEsc = false;
    bool allowPrintScreen = false;
    bool allowSystemKey = false;
    bool allowActionCenterCombo = true;

    QJsonObject toJson() const
    {
        return QJsonObject{
            {QStringLiteral("allowAltTab"), allowAltTab},
            {QStringLiteral("allowAltF4"), allowAltF4},
            {QStringLiteral("allowAltEsc"), allowAltEsc},
            {QStringLiteral("allowCtrlEsc"), allowCtrlEsc},
            {QStringLiteral("allowPrintScreen"), allowPrintScreen},
            {QStringLiteral("allowSystemKey"), allowSystemKey},
            {QStringLiteral("allowActionCenterCombo"), allowActionCenterCombo},
        };
    }

    static KeyboardLockPolicy fromJson(const QJsonObject &obj)
    {
        KeyboardLockPolicy policy;
        policy.allowAltTab = obj.value(QStringLiteral("allowAltTab")).toBool(false);
        policy.allowAltF4 = obj.value(QStringLiteral("allowAltF4")).toBool(false);
        policy.allowAltEsc = obj.value(QStringLiteral("allowAltEsc")).toBool(false);
        policy.allowCtrlEsc = obj.value(QStringLiteral("allowCtrlEsc")).toBool(false);
        policy.allowPrintScreen = obj.value(QStringLiteral("allowPrintScreen")).toBool(false);
        policy.allowSystemKey = obj.value(QStringLiteral("allowSystemKey")).toBool(false);
        policy.allowActionCenterCombo = obj.value(QStringLiteral("allowActionCenterCombo")).toBool(true);
        return policy;
    }
};

inline QStringList defaultProhibitedProcesses()
{
    return {
        // External web browsers
        QStringLiteral("firefox"),
        QStringLiteral("firefox-bin"),
        QStringLiteral("chromium"),
        QStringLiteral("chrome"),
        QStringLiteral("google-chrome"),
        QStringLiteral("google-chrome-stable"),
        QStringLiteral("brave"),
        QStringLiteral("brave-browser"),
        QStringLiteral("vivaldi-bin"),
        QStringLiteral("opera"),
        QStringLiteral("zen-bin"),
        QStringLiteral("msedge"),
        QStringLiteral("microsoft-edge"),
        // Chat / communication
        QStringLiteral("discord"),
        QStringLiteral("vesktop"),
        QStringLiteral("telegram-desktop"),
        QStringLiteral("slack"),
        QStringLiteral("teams"),
        QStringLiteral("teams-for-linux"),
        // Screen recorders / capture / remote desktop
        QStringLiteral("obs"),
        QStringLiteral("wf-recorder"),
        QStringLiteral("simplescreenrecorder"),
        QStringLiteral("kazam"),
        QStringLiteral("kooha"),
        QStringLiteral("spectacle"),
        QStringLiteral("flameshot"),
        QStringLiteral("anydesk"),
        QStringLiteral("teamviewer"),
        QStringLiteral("rustdesk"),
        QStringLiteral("vncviewer"),
        QStringLiteral("x11vnc"),
        QStringLiteral("remmina"),
        // Debuggers / tracers
        QStringLiteral("gdb"),
        QStringLiteral("lldb"),
        QStringLiteral("strace"),
        QStringLiteral("ltrace"),
        QStringLiteral("frida-server"),
    };
}

inline QJsonArray toJsonArray(const QStringList &items)
{
    QJsonArray array;
    for (const QString &item : items) {
        array.append(item);
    }
    return array;
}

inline QStringList fromJsonArray(const QJsonArray &array)
{
    QStringList items;
    items.reserve(array.size());
    for (const QJsonValue &value : array) {
        const QString text = value.toString().trimmed();
        if (!text.isEmpty()) {
            items.append(text);
        }
    }
    return items;
}

}  // namespace seb::lockdown
