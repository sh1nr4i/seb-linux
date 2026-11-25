#include "keyboard_blocker.h"

#include <QProcess>
#include <QSocketNotifier>

#include <cerrno>
#include <cstring>
#include <dirent.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <sys/ioctl.h>
#include <unistd.h>

namespace seb::lockdown {

namespace {

bool testBit(const unsigned char *bits, std::size_t bit)
{
    return (bits[bit / 8] & (1u << (bit % 8))) != 0;
}

bool isFunctionKey(std::uint16_t code)
{
    return (code >= KEY_F1 && code <= KEY_F10) ||
           code == KEY_F11 ||
           code == KEY_F12 ||
           (code >= KEY_F13 && code <= KEY_F24);
}

bool isSystemLauncherKey(std::uint16_t code)
{
    switch (code) {
    case KEY_MENU:
    case KEY_COMPOSE:
    case KEY_SLEEP:
    case KEY_POWER:
    case KEY_SUSPEND:
    case KEY_SCREENLOCK:
    case KEY_SEARCH:
    case KEY_HOMEPAGE:
    case KEY_CALC:
    case KEY_FILE:
    case KEY_WWW:
    case KEY_DASHBOARD:
    case KEY_SCALE:
    case KEY_SWITCHVIDEOMODE:
    case KEY_PROG1:
    case KEY_PROG2:
    case KEY_PROG3:
    case KEY_PROG4:
    case KEY_BOOKMARKS:
    case KEY_COMPUTER:
    case KEY_MAIL:
    case KEY_CHAT:
    case KEY_HELP:
    case KEY_OPEN:
    case KEY_FIND:
        return true;
    default:
        return false;
    }
}

bool isAltAllowedKey(std::uint16_t code)
{
    // Only allow browser navigation combos with Alt (Back/Forward/Home).
    return code == KEY_LEFT || code == KEY_RIGHT || code == KEY_HOME;
}

}  // namespace

KeyboardBlocker::KeyboardBlocker(QObject *parent)
    : QObject(parent)
{
}

KeyboardBlocker::~KeyboardBlocker()
{
    unlock();
}

bool KeyboardBlocker::lock(const KeyboardLockPolicy &policy, QString *error)
{
    policy_ = policy;
    if (locked_) {
        return true;
    }

    resetModifierState();
    suppressedKeys_.clear();

    if (!createVirtualKeyboard(error)) {
        return false;
    }

    // Give udev/libinput a brief moment to register the virtual keyboard before grabbing physical ones.
    ::usleep(80000);

    DIR *dir = ::opendir("/dev/input");
    if (!dir) {
        destroyVirtualKeyboard();
        if (error) {
            *error = QStringLiteral("Unable to open /dev/input directory: %1")
                         .arg(QString::fromLocal8Bit(std::strerror(errno)));
        }
        return false;
    }

    while (struct dirent *ent = ::readdir(dir)) {
        if (std::strncmp(ent->d_name, "event", 5) != 0) {
            continue;
        }
        const QString fullPath = QStringLiteral("/dev/input/") + QString::fromLocal8Bit(ent->d_name);
        const QByteArray encodedPath = fullPath.toLocal8Bit();
        const int fd = ::open(encodedPath.constData(), O_RDONLY | O_NONBLOCK | O_CLOEXEC);
        if (fd < 0) {
            continue;
        }

        if (!isPhysicalKeyboard(fd)) {
            ::close(fd);
            continue;
        }

        if (::ioctl(fd, EVIOCGRAB, 1) < 0) {
            ::close(fd);
            continue;
        }

        auto *notifier = new QSocketNotifier(fd, QSocketNotifier::Read, this);
        connect(notifier, &QSocketNotifier::activated, this, [this, fd] {
            handleDeviceReadable(fd);
        });
        devices_.append(GrabbedDevice{fd, notifier, fullPath});
    }
    ::closedir(dir);

    if (devices_.isEmpty()) {
        destroyVirtualKeyboard();
        if (error) {
            *error = QStringLiteral("No physical keyboard input devices could be locked via evdev.");
        }
        return false;
    }

    locked_ = true;
    return true;
}

void KeyboardBlocker::updatePolicy(const KeyboardLockPolicy &policy)
{
    policy_ = policy;
}

void KeyboardBlocker::unlock()
{
    locked_ = false;

    for (auto &device : devices_) {
        if (device.notifier) {
            device.notifier->setEnabled(false);
            delete device.notifier;
            device.notifier = nullptr;
        }
        if (device.fd >= 0) {
            ::ioctl(device.fd, EVIOCGRAB, 0);
            ::close(device.fd);
            device.fd = -1;
        }
    }
    devices_.clear();

    destroyVirtualKeyboard();
    suppressedKeys_.clear();
    resetModifierState();
}

bool KeyboardBlocker::createVirtualKeyboard(QString *error)
{
    auto openUinput = []() -> int {
        int fd = ::open("/dev/uinput", O_WRONLY | O_NONBLOCK | O_CLOEXEC);
        if (fd < 0) {
            fd = ::open("/dev/input/uinput", O_WRONLY | O_NONBLOCK | O_CLOEXEC);
        }
        return fd;
    };

    uinputFd_ = openUinput();
    if (uinputFd_ < 0 && errno == ENOENT) {
        QProcess::execute(QStringLiteral("modprobe"), {QStringLiteral("uinput")});
        ::usleep(100000);
        uinputFd_ = openUinput();
    }
    if (uinputFd_ < 0) {
        if (error) {
            *error = QStringLiteral("Unable to open /dev/uinput: %1")
                         .arg(QString::fromLocal8Bit(std::strerror(errno)));
        }
        return false;
    }

    if (::ioctl(uinputFd_, UI_SET_EVBIT, EV_KEY) < 0 ||
        ::ioctl(uinputFd_, UI_SET_EVBIT, EV_SYN) < 0) {
        if (error) {
            *error = QStringLiteral("Failed to configure /dev/uinput event types.");
        }
        destroyVirtualKeyboard();
        return false;
    }

    // Register standard keyboard keys (1..248) only — avoid BTN_* ranges (0x100..0x1ff)
    // so libinput/Wayland always classifies this device strictly as a keyboard.
    for (int code = 1; code <= 248; ++code) {
        ::ioctl(uinputFd_, UI_SET_KEYBIT, code);
    }

    struct uinput_setup setup{};
    setup.id.bustype = BUS_USB;
    setup.id.vendor = 0x1d6b;
    setup.id.product = 0x5eb1;
    setup.id.version = 1;
    std::strncpy(setup.name, kVirtualKeyboardName, UINPUT_MAX_NAME_SIZE - 1);
    setup.name[UINPUT_MAX_NAME_SIZE - 1] = '\0';

    if (::ioctl(uinputFd_, UI_DEV_SETUP, &setup) < 0 ||
        ::ioctl(uinputFd_, UI_DEV_CREATE) < 0) {
        if (error) {
            *error = QStringLiteral("Failed to create virtual keyboard device via /dev/uinput.");
        }
        destroyVirtualKeyboard();
        return false;
    }

    return true;
}

void KeyboardBlocker::destroyVirtualKeyboard()
{
    if (uinputFd_ >= 0) {
        ::ioctl(uinputFd_, UI_DEV_DESTROY);
        ::close(uinputFd_);
        uinputFd_ = -1;
    }
}

bool KeyboardBlocker::isPhysicalKeyboard(int fd) const
{
    char name[256]{};
    if (::ioctl(fd, EVIOCGNAME(sizeof(name) - 1), name) >= 0) {
        if (std::strcmp(name, kVirtualKeyboardName) == 0) {
            return false;
        }
    }

    unsigned char evBits[(EV_MAX / 8) + 1]{};
    if (::ioctl(fd, EVIOCGBIT(0, sizeof(evBits)), evBits) < 0) {
        return false;
    }
    if (!testBit(evBits, EV_KEY)) {
        return false;
    }

    // Never grab mice, touchpads, or pointers.
    if (testBit(evBits, EV_REL)) {
        unsigned char relBits[(REL_MAX / 8) + 1]{};
        if (::ioctl(fd, EVIOCGBIT(EV_REL, sizeof(relBits)), relBits) >= 0 &&
            (testBit(relBits, REL_X) || testBit(relBits, REL_Y))) {
            return false;
        }
    }
    if (testBit(evBits, EV_ABS)) {
        unsigned char absBits[(ABS_MAX / 8) + 1]{};
        if (::ioctl(fd, EVIOCGBIT(EV_ABS, sizeof(absBits)), absBits) >= 0 &&
            (testBit(absBits, ABS_X) || testBit(absBits, ABS_MT_POSITION_X))) {
            return false;
        }
    }

    unsigned char keyBits[(KEY_MAX / 8) + 1]{};
    if (::ioctl(fd, EVIOCGBIT(EV_KEY, sizeof(keyBits)), keyBits) < 0) {
        return false;
    }

    const bool mainKeyboard =
        (testBit(keyBits, KEY_A) && testBit(keyBits, KEY_ENTER)) ||
        (testBit(keyBits, KEY_TAB) && (testBit(keyBits, KEY_LEFTALT) || testBit(keyBits, KEY_RIGHTALT)));
    const bool hotkeyKeyboard =
        testBit(keyBits, KEY_LEFTMETA) ||
        testBit(keyBits, KEY_SYSRQ) ||
        testBit(keyBits, KEY_SWITCHVIDEOMODE) ||
        testBit(keyBits, KEY_SCREENLOCK) ||
        testBit(keyBits, KEY_CALC) ||
        testBit(keyBits, KEY_SEARCH);
    return mainKeyboard || hotkeyKeyboard;
}

void KeyboardBlocker::handleDeviceReadable(int fd)
{
    struct input_event events[32]{};
    while (true) {
        const ssize_t bytes = ::read(fd, events, sizeof(events));
        if (bytes < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                break;
            }
            if (errno == ENODEV) {
                for (qsizetype i = 0; i < devices_.size(); ++i) {
                    if (devices_[i].fd == fd) {
                        if (devices_[i].notifier) {
                            devices_[i].notifier->setEnabled(false);
                            devices_[i].notifier->deleteLater();
                        }
                        ::close(fd);
                        devices_.removeAt(i);
                        break;
                    }
                }
            }
            break;
        }
        if (bytes == 0) {
            break;
        }

        const std::size_t count = static_cast<std::size_t>(bytes) / sizeof(struct input_event);
        for (std::size_t i = 0; i < count; ++i) {
            processInputEvent(events[i]);
        }
    }
}

void KeyboardBlocker::processInputEvent(const struct input_event &ev)
{
    if (uinputFd_ < 0) {
        return;
    }

    if (ev.type == EV_KEY) {
        const std::uint16_t code = ev.code;
        const std::int32_t value = ev.value;
        const bool pressed = (value != 0);

        switch (code) {
        case KEY_LEFTCTRL: leftCtrl_ = pressed; break;
        case KEY_RIGHTCTRL: rightCtrl_ = pressed; break;
        case KEY_LEFTALT: leftAlt_ = pressed; break;
        case KEY_RIGHTALT: rightAlt_ = pressed; break;
        case KEY_LEFTSHIFT: leftShift_ = pressed; break;
        case KEY_RIGHTSHIFT: rightShift_ = pressed; break;
        case KEY_LEFTMETA: leftMeta_ = pressed; break;
        case KEY_RIGHTMETA: rightMeta_ = pressed; break;
        default: break;
        }

        if (altTabActive_ && (code == KEY_LEFTALT || code == KEY_RIGHTALT) &&
            value == 0 && !leftAlt_ && !rightAlt_) {
            altTabActive_ = false;
            emit altTabCommitted();
        }

        if (shouldBlockKey(code, value)) {
            return;
        }

        emitKeyEvent(code, value);
        emitSynReport();
        return;
    }

    if (ev.type == EV_SYN && ev.code == SYN_REPORT) {
        emitSynReport();
    }
}

bool KeyboardBlocker::shouldBlockKey(std::uint16_t code, std::int32_t value)
{
    const bool ctrl = leftCtrl_ || rightCtrl_;
    const bool alt = leftAlt_ || rightAlt_;
    const bool meta = leftMeta_ || rightMeta_;

    // Never forward Meta/Super itself when system keys are disabled,
    // so tapping Super never opens the desktop launcher.
    if (!policy_.allowSystemKey && (code == KEY_LEFTMETA || code == KEY_RIGHTMETA)) {
        return true;
    }

    // Release of any previously suppressed key must also be dropped cleanly.
    if (value == 0 && suppressedKeys_.remove(code)) {
        return true;
    }

    // Cancel internal SEB Alt+Tab overlay if Esc is pressed while holding Alt.
    if (altTabActive_ && code == KEY_ESC) {
        if (value == 1) {
            suppressedKeys_.insert(code);
            altTabActive_ = false;
            emit altTabCancelled();
        }
        return true;
    }

    // Intercept OS Alt+Tab / Alt+Shift+Tab and drive SEB's internal window switcher instead.
    if (!policy_.allowAltTab && alt && !ctrl && !meta && code == KEY_TAB) {
        if (value == 1 || value == 2) {
            suppressedKeys_.insert(code);
            altTabActive_ = true;
            emit altTabStepped(leftShift_ || rightShift_);
        }
        return true;
    }

    // Intercept Super+A and translate it to Ctrl+Shift+A for SEB Action Center.
    if (!policy_.allowSystemKey && meta && code == KEY_A) {
        if (value == 1) {
            suppressedKeys_.insert(code);
            if (policy_.allowActionCenterCombo) {
                emitActionCenterCombo();
            }
        }
        return true;
    }

    // Block all other Super/Meta combinations when system keys are disabled.
    if (!policy_.allowSystemKey && meta) {
        if (value != 0) {
            suppressedKeys_.insert(code);
        }
        return true;
    }

    // Block ALL Ctrl+Alt+<any key> OS combos (TTY switch Ctrl+Alt+F1..F12,
    // Ctrl+Alt+Del, Ctrl+Alt+T terminal, Ctrl+Alt+L lock, Ctrl+Alt+Arrows, Ctrl+Alt+Esc, etc.).
    if (ctrl && alt && code != KEY_LEFTCTRL && code != KEY_RIGHTCTRL &&
        code != KEY_LEFTALT && code != KEY_RIGHTALT &&
        code != KEY_LEFTSHIFT && code != KEY_RIGHTSHIFT) {
        if (value != 0) {
            suppressedKeys_.insert(code);
        }
        return true;
    }

    // Block ALL LeftAlt/Alt OS shortcuts (Alt+Tab, Alt+Shift+Tab, Alt+F1..F24 including
    // KDE Alt+F2 KRunner & Alt+F3 Window Menu, Alt+Space, Alt+Grave/~, Alt+Esc, etc.)
    // except explicit browser navigation keys (Alt+Left, Alt+Right, Alt+Home).
    if (alt && code != KEY_LEFTALT && code != KEY_RIGHTALT &&
        code != KEY_LEFTSHIFT && code != KEY_RIGHTSHIFT &&
        code != KEY_LEFTCTRL && code != KEY_RIGHTCTRL) {
        const bool allowedAltCombo =
            (policy_.allowAltTab && code == KEY_TAB) ||
            (policy_.allowAltF4 && code == KEY_F4) ||
            (policy_.allowAltEsc && code == KEY_ESC) ||
            (!leftAlt_ && rightAlt_ && !isFunctionKey(code) &&
             code != KEY_TAB && code != KEY_SPACE && code != KEY_ESC && code != KEY_GRAVE) ||
            isAltAllowedKey(code);
        if (!allowedAltCombo) {
            if (value != 0) {
                suppressedKeys_.insert(code);
            }
            return true;
        }
    }

    // Block desktop-environment Ctrl shortcuts:
    // Ctrl+F1..F12 (KDE virtual desktop / Overview / Desktop Grid / Present Windows),
    // Ctrl+Esc / Ctrl+Shift+Esc (System Activity / Task Manager).
    if (ctrl && (isFunctionKey(code) || (!policy_.allowCtrlEsc && code == KEY_ESC))) {
        if (value != 0) {
            suppressedKeys_.insert(code);
        }
        return true;
    }

    // Block standalone PrintScreen / SysRq and OS launcher/hotkey keys.
    const bool blocked =
        (!policy_.allowPrintScreen && (code == KEY_SYSRQ || code == KEY_PRINT)) ||
        (!policy_.allowSystemKey && isSystemLauncherKey(code));

    if (blocked) {
        if (value != 0) {
            suppressedKeys_.insert(code);
        }
        return true;
    }

    return false;
}

void KeyboardBlocker::emitKeyEvent(std::uint16_t code, std::int32_t value)
{
    struct input_event ev{};
    ev.type = EV_KEY;
    ev.code = code;
    ev.value = value;
    const ssize_t ignored = ::write(uinputFd_, &ev, sizeof(ev));
    Q_UNUSED(ignored);
}

void KeyboardBlocker::emitSynReport()
{
    struct input_event ev{};
    ev.type = EV_SYN;
    ev.code = SYN_REPORT;
    ev.value = 0;
    const ssize_t ignored = ::write(uinputFd_, &ev, sizeof(ev));
    Q_UNUSED(ignored);
}

void KeyboardBlocker::emitActionCenterCombo()
{
    const bool needCtrl = !(leftCtrl_ || rightCtrl_);
    const bool needShift = !(leftShift_ || rightShift_);

    if (needCtrl) emitKeyEvent(KEY_LEFTCTRL, 1);
    if (needShift) emitKeyEvent(KEY_LEFTSHIFT, 1);
    emitKeyEvent(KEY_A, 1);
    emitSynReport();

    emitKeyEvent(KEY_A, 0);
    if (needShift) emitKeyEvent(KEY_LEFTSHIFT, 0);
    if (needCtrl) emitKeyEvent(KEY_LEFTCTRL, 0);
    emitSynReport();
}

void KeyboardBlocker::resetModifierState()
{
    leftCtrl_ = false;
    rightCtrl_ = false;
    leftAlt_ = false;
    rightAlt_ = false;
    leftShift_ = false;
    rightShift_ = false;
    leftMeta_ = false;
    rightMeta_ = false;
    altTabActive_ = false;
}

}  // namespace seb::lockdown
