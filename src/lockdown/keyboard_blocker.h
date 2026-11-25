#pragma once

#include "lockdown_protocol.h"

#include <QList>
#include <QObject>
#include <QSet>
#include <QString>

#include <cstdint>

QT_BEGIN_NAMESPACE
class QSocketNotifier;
QT_END_NAMESPACE

struct input_event;

namespace seb::lockdown {

class KeyboardBlocker : public QObject
{
    Q_OBJECT

public:
    explicit KeyboardBlocker(QObject *parent = nullptr);
    ~KeyboardBlocker() override;

    bool lock(const KeyboardLockPolicy &policy, QString *error = nullptr);
    void updatePolicy(const KeyboardLockPolicy &policy);
    void unlock();

    bool isLocked() const { return locked_; }
    int grabbedDeviceCount() const { return static_cast<int>(devices_.size()); }

signals:
    void altTabStepped(bool backwards);
    void altTabCommitted();
    void altTabCancelled();

private:
    struct GrabbedDevice
    {
        int fd = -1;
        QSocketNotifier *notifier = nullptr;
        QString path;
    };

    bool createVirtualKeyboard(QString *error);
    void destroyVirtualKeyboard();
    bool isPhysicalKeyboard(int fd) const;
    void handleDeviceReadable(int fd);
    void processInputEvent(const struct input_event &ev);
    void emitKeyEvent(std::uint16_t code, std::int32_t value);
    void emitSynReport();
    void emitActionCenterCombo();
    bool shouldBlockKey(std::uint16_t code, std::int32_t value);
    void resetModifierState();

    KeyboardLockPolicy policy_;
    bool locked_ = false;
    int uinputFd_ = -1;
    QList<GrabbedDevice> devices_;
    QSet<std::uint16_t> suppressedKeys_;

    bool leftCtrl_ = false;
    bool rightCtrl_ = false;
    bool leftAlt_ = false;
    bool rightAlt_ = false;
    bool leftShift_ = false;
    bool rightShift_ = false;
    bool leftMeta_ = false;
    bool rightMeta_ = false;
    bool altTabActive_ = false;
};

}  // namespace seb::lockdown
