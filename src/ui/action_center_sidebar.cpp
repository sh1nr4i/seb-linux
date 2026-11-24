#include "action_center_sidebar.h"

#include "taskbar/common/taskbar_style.h"
#include "../browser_window.h"
#include "../seb_session.h"
#include "../shell/taskbar/platform/audio_controller.h"
#include "../shell/taskbar/platform/battery_controller.h"
#include "../shell/taskbar/platform/keyboard_controller.h"
#include "../shell/taskbar/platform/network_controller.h"

#include <QDateTime>
#include <QFrame>
#include <QGridLayout>
#include <QHideEvent>
#include <QIcon>
#include <QKeyEvent>
#include <QLabel>
#include <QLocale>
#include <QMessageBox>
#include <QPainter>
#include <QPalette>
#include <QPixmap>
#include <QPushButton>
#include <QScrollArea>
#include <QTimer>
#include <QVBoxLayout>

namespace seb::ui {

class ActionCenterTile : public QPushButton
{
public:
    explicit ActionCenterTile(QWidget *parent = nullptr)
        : QPushButton(parent)
    {
        setCursor(Qt::PointingHandCursor);
        setFocusPolicy(Qt::NoFocus);
        setFixedHeight(78);
        setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
        setStyleSheet(QStringLiteral(
            "QPushButton { background: lightgray; color: black; border: 1px solid gray; padding: 0; }"
            "QPushButton:hover { background: %1; border: 1px solid %2; color: black; }"
            "QPushButton:pressed { background: %1; border: 2px solid %3; color: black; }"
            "QPushButton:disabled { background: lightgray; color: rgba(0, 0, 0, 64); border: 1px solid gray; }")
                          .arg(taskbar::hoverBackgroundColor().name(),
                               taskbar::hoverBorderColor().name(),
                               taskbar::pressedBorderColor().name()));

        auto *layout = new QVBoxLayout(this);
        layout->setContentsMargins(4, 7, 4, 5);
        layout->setSpacing(4);

        iconLabel_ = new QLabel(this);
        iconLabel_->setAlignment(Qt::AlignCenter);
        iconLabel_->setAttribute(Qt::WA_TransparentForMouseEvents, true);
        iconLabel_->setStyleSheet(QStringLiteral("background: transparent; border: none;"));
        layout->addWidget(iconLabel_);

        textLabel_ = new QLabel(this);
        textLabel_->setAlignment(Qt::AlignHCenter | Qt::AlignVCenter);
        textLabel_->setWordWrap(true);
        textLabel_->setAttribute(Qt::WA_TransparentForMouseEvents, true);
        textLabel_->setStyleSheet(QStringLiteral(
            "background: transparent; border: none; color: black; font-size: 10px;"));
        layout->addWidget(textLabel_, 1);
    }

    void setTileIcon(const QIcon &icon)
    {
        iconLabel_->setPixmap(icon.pixmap(QSize(22, 22)));
    }

    void setTileText(const QString &text)
    {
        textLabel_->setText(text);
    }

private:
    QLabel *iconLabel_ = nullptr;
    QLabel *textLabel_ = nullptr;
};

namespace {

constexpr int kSidebarWidth = 368;
constexpr int kMaxGridColumns = 4;

QIcon buildKeyboardIcon()
{
    QPixmap pixmap(44, 44);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(QPen(Qt::black, 2.4, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    p.setBrush(Qt::NoBrush);
    p.drawRoundedRect(QRectF(4, 11, 36, 22), 3, 3);
    p.setPen(Qt::NoPen);
    p.setBrush(Qt::black);
    for (int col = 0; col < 5; ++col) {
        p.drawRect(QRectF(8 + col * 6, 15, 4, 3));
        p.drawRect(QRectF(8 + col * 6, 20, 4, 3));
    }
    p.drawRect(QRectF(12, 26, 20, 3));
    return QIcon(pixmap);
}

QIcon buildClockIcon()
{
    QPixmap pixmap(44, 44);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(QPen(Qt::black, 2.8, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    p.setBrush(Qt::NoBrush);
    p.drawEllipse(QRectF(6, 6, 32, 32));
    p.drawLine(QPointF(22, 22), QPointF(22, 13));
    p.drawLine(QPointF(22, 22), QPointF(29, 25));
    return QIcon(pixmap);
}

QIcon buildBatteryTileIcon(const seb::shell::taskbar::platform::BatteryState &state)
{
    QPixmap pixmap(44, 44);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, true);

    const QRectF body(4.0, 12.0, 32.0, 20.0);
    const QRectF terminal(36.0, 17.0, 4.0, 10.0);

    const int clamped = qBound(0, state.percentage, 100);
    if (state.available && clamped > 0) {
        const double innerWidth = (body.width() - 6.0) * (clamped / 100.0);
        p.setPen(Qt::NoPen);
        p.setBrush(Qt::black);
        p.drawRect(QRectF(body.left() + 3.0, body.top() + 3.0, innerWidth, body.height() - 6.0));
    }

    p.setPen(QPen(Qt::black, 2.6, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    p.setBrush(Qt::NoBrush);
    p.drawRoundedRect(body, 2.5, 2.5);

    p.setPen(Qt::NoPen);
    p.setBrush(Qt::black);
    p.drawRoundedRect(terminal, 1.0, 1.0);
    return QIcon(pixmap);
}

QString iconPathForNetwork(const seb::shell::taskbar::platform::NetworkState &state)
{
    if (!state.available) return QStringLiteral(":/assets/taskbar/wifi_0.svg");
    if (state.type == seb::shell::taskbar::platform::NetworkState::Type::Wired) {
        return QStringLiteral(":/assets/taskbar/network_wired.svg");
    }
    if (state.status != seb::shell::taskbar::platform::NetworkState::Status::Connected) {
        return QStringLiteral(":/assets/taskbar/wifi_0.svg");
    }
    int strength = 0;
    for (const auto &net : state.networks) {
        if (net.active) {
            strength = net.signalPercent;
            break;
        }
    }
    if (strength > 66) return QStringLiteral(":/assets/taskbar/wifi_100.svg");
    if (strength > 33) return QStringLiteral(":/assets/taskbar/wifi_66.svg");
    if (strength > 0) return QStringLiteral(":/assets/taskbar/wifi_33.svg");
    return QStringLiteral(":/assets/taskbar/wifi_0.svg");
}

QString iconPathForAudio(const seb::shell::taskbar::platform::AudioState &state)
{
    if (!state.available) return QStringLiteral(":/assets/taskbar/audio_no_device.svg");
    if (state.muted) return QStringLiteral(":/assets/taskbar/audio_muted.svg");
    if (state.volumePercent > 66) return QStringLiteral(":/assets/taskbar/audio_100.svg");
    if (state.volumePercent > 33) return QStringLiteral(":/assets/taskbar/audio_66.svg");
    return QStringLiteral(":/assets/taskbar/audio_33.svg");
}

}  // namespace

ActionCenterSidebar::ActionCenterSidebar(SebSession &session, QWidget *parent)
    : QFrame(parent, Qt::Popup | Qt::FramelessWindowHint)
    , session_(session)
{
    setAttribute(Qt::WA_DeleteOnClose, false);
    setFocusPolicy(Qt::StrongFocus);
    setFixedWidth(kSidebarWidth);
    setWindowOpacity(0.95);
    setObjectName(QStringLiteral("sebActionCenterPanel"));
    setAttribute(Qt::WA_StyledBackground, true);

    audioController_ = new seb::shell::taskbar::platform::AudioController(this);
    keyboardController_ = new seb::shell::taskbar::platform::KeyboardController(this);

    QPalette lightPalette = palette();
    lightPalette.setColor(QPalette::Window, taskbar::backgroundColor());
    lightPalette.setColor(QPalette::Base, taskbar::backgroundColor());
    lightPalette.setColor(QPalette::Button, taskbar::backgroundColor());
    lightPalette.setColor(QPalette::WindowText, Qt::black);
    lightPalette.setColor(QPalette::Text, Qt::black);
    lightPalette.setColor(QPalette::ButtonText, Qt::black);
    setPalette(lightPalette);

    setStyleSheet(QStringLiteral(
        "#sebActionCenterPanel { background: %1; border-right: 1px solid gray; color: black; }")
                      .arg(taskbar::backgroundColor().name()));

    auto *panelLayout = new QVBoxLayout(this);
    panelLayout->setContentsMargins(10, 10, 10, 10);
    panelLayout->setSpacing(6);

    // Top section: "Browser" title + list of open windows (matching SEB Action Center)
    auto *headerLabel = new QLabel(QStringLiteral("Browser"), this);
    headerLabel->setAlignment(Qt::AlignHCenter | Qt::AlignVCenter);
    headerLabel->setStyleSheet(QStringLiteral(
        "color: black; font-size: 13px; font-weight: 700; padding: 4px 0 6px 0; background: transparent;"));
    panelLayout->addWidget(headerLabel);

    auto *windowsScroll = new QScrollArea(this);
    windowsScroll->setFrameShape(QFrame::NoFrame);
    windowsScroll->setWidgetResizable(true);
    windowsScroll->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    windowsScroll->setStyleSheet(QStringLiteral(
        "QScrollArea { background: transparent; border: 0; }"
        "QScrollBar:vertical { background: #d8d8d8; width: 8px; margin: 2px; }"
        "QScrollBar::handle:vertical { background: #b0b0b0; min-height: 20px; }"
        "QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height: 0; }"));

    auto *windowsContainer = new QWidget(windowsScroll);
    windowsContainer->setStyleSheet(QStringLiteral("background: transparent;"));
    auto *windowsOuter = new QVBoxLayout(windowsContainer);
    windowsOuter->setContentsMargins(0, 0, 0, 0);
    windowsOuter->setSpacing(0);

    windowsLayout_ = new QVBoxLayout();
    windowsLayout_->setContentsMargins(0, 0, 0, 0);
    windowsLayout_->setSpacing(2);
    windowsOuter->addLayout(windowsLayout_);
    windowsOuter->addStretch(1);

    windowsScroll->setWidget(windowsContainer);
    panelLayout->addWidget(windowsScroll, 1);

    // Bottom section: Shortcut tile table (max 4 columns)
    auto *shortcutsContainer = new QWidget(this);
    shortcutsContainer->setStyleSheet(QStringLiteral("background: transparent;"));
    shortcutsGrid_ = new QGridLayout(shortcutsContainer);
    shortcutsGrid_->setContentsMargins(0, 4, 0, 0);
    shortcutsGrid_->setHorizontalSpacing(6);
    shortcutsGrid_->setVerticalSpacing(6);
    for (int col = 0; col < kMaxGridColumns; ++col) {
        shortcutsGrid_->setColumnStretch(col, 1);
    }
    panelLayout->addWidget(shortcutsContainer);

    rebuildShortcutGrid();

    clockTimer_ = new QTimer(this);
    clockTimer_->setInterval(1000);
    connect(clockTimer_, &QTimer::timeout, this, &ActionCenterSidebar::refreshClock);

    connect(&session_, &SebSession::browserWindowsChanged, this, [this] {
        if (isVisible()) {
            refreshWindows();
        }
    });
    if (auto *battery = session_.batteryController()) {
        connect(battery, &seb::shell::taskbar::platform::BatteryController::stateChanged, this, [this] {
            if (isVisible()) {
                rebuildShortcutGrid();
                refreshDynamicTiles();
            }
        });
    }
    if (auto *network = session_.networkController()) {
        connect(network, &seb::shell::taskbar::platform::NetworkController::stateChanged, this, [this] {
            if (isVisible()) {
                refreshDynamicTiles();
            }
        });
    }
    connect(audioController_, &seb::shell::taskbar::platform::AudioController::stateChanged, this, [this] {
        if (isVisible()) {
            refreshDynamicTiles();
        }
    });
    connect(keyboardController_, &seb::shell::taskbar::platform::KeyboardController::stateChanged, this, [this] {
        if (isVisible()) {
            refreshDynamicTiles();
        }
    });
}

void ActionCenterSidebar::rebuildShortcutGrid()
{
    if (!shortcutsGrid_) {
        return;
    }

    QLayoutItem *item = nullptr;
    while ((item = shortcutsGrid_->takeAt(0)) != nullptr) {
        delete item->widget();
        delete item;
    }
    keyboardTile_ = nullptr;
    networkTile_ = nullptr;
    audioTile_ = nullptr;
    batteryTile_ = nullptr;
    clockTile_ = nullptr;

    const auto &settings = session_.settings();
    const auto &actionCenter = settings.UserInterface.ActionCenter;

    int index = 0;
    const auto addTile = [&](ActionCenterTile *tile) {
        shortcutsGrid_->addWidget(tile, index / kMaxGridColumns, index % kMaxGridColumns);
        ++index;
    };

    if (actionCenter.ShowApplicationInfo) {
        auto *infoTile = new ActionCenterTile(this);
        infoTile->setTileIcon(QIcon(QStringLiteral(":/assets/taskbar/about.svg")));
        infoTile->setTileText(QStringLiteral("Information\nabout SEB"));
        connect(infoTile, &QPushButton::clicked, this, &ActionCenterSidebar::showAboutDialog);
        addTile(infoTile);
    }

    if (actionCenter.ShowKeyboardLayout) {
        keyboardTile_ = new ActionCenterTile(this);
        keyboardTile_->setTileIcon(buildKeyboardIcon());
        keyboardTile_->setTileText(QStringLiteral("Keyboard"));
        addTile(keyboardTile_);
    }

    if (actionCenter.ShowNetwork) {
        networkTile_ = new ActionCenterTile(this);
        networkTile_->setTileIcon(QIcon(QStringLiteral(":/assets/taskbar/wifi_0.svg")));
        networkTile_->setTileText(QStringLiteral("Network"));
        addTile(networkTile_);
    }

    if (actionCenter.ShowAudio) {
        audioTile_ = new ActionCenterTile(this);
        audioTile_->setTileIcon(QIcon(QStringLiteral(":/assets/taskbar/audio_100.svg")));
        audioTile_->setTileText(QStringLiteral("Audio"));
        connect(audioTile_, &QPushButton::clicked, this, [this] {
            if (audioController_ && audioController_->state().available) {
                audioController_->setMuted(!audioController_->state().muted);
            }
        });
        addTile(audioTile_);
    }

    if (auto *battery = session_.batteryController(); battery && battery->state().available) {
        batteryTile_ = new ActionCenterTile(this);
        addTile(batteryTile_);
    }

    if (actionCenter.ShowClock) {
        clockTile_ = new ActionCenterTile(this);
        clockTile_->setTileIcon(buildClockIcon());
        addTile(clockTile_);
    }

    auto *terminateTile = new ActionCenterTile(this);
    terminateTile->setTileIcon(QIcon(QStringLiteral(":/assets/taskbar/shutdown.svg")));
    terminateTile->setTileText(QStringLiteral("Terminate\nSession"));
    terminateTile->setEnabled(settings.Security.AllowTermination);
    connect(terminateTile, &QPushButton::clicked, this, [this] {
        closeSidebar();
        emit quitRequested();
    });
    addTile(terminateTile);
}

void ActionCenterSidebar::toggleSidebar()
{
    if (isVisible()) {
        closeSidebar();
    } else {
        openSidebar();
    }
}

void ActionCenterSidebar::openSidebar()
{
    if (parentWidget()) {
        setFixedSize(kSidebarWidth, parentWidget()->height());
        move(parentWidget()->mapToGlobal(QPoint(0, 0)));
    }
    rebuildShortcutGrid();
    refreshWindows();
    refreshDynamicTiles();
    refreshClock();
    if (clockTile_ && !clockTimer_->isActive()) {
        clockTimer_->start();
    }
    show();
    raise();
    activateWindow();
    setFocus(Qt::PopupFocusReason);
}

void ActionCenterSidebar::closeSidebar()
{
    if (clockTimer_->isActive()) {
        clockTimer_->stop();
    }
    hide();
}

void ActionCenterSidebar::hideEvent(QHideEvent *event)
{
    if (clockTimer_ && clockTimer_->isActive()) {
        clockTimer_->stop();
    }
    QFrame::hideEvent(event);
}

void ActionCenterSidebar::keyPressEvent(QKeyEvent *event)
{
    if (!event) {
        QFrame::keyPressEvent(event);
        return;
    }
    const bool meta = event->modifiers().testFlag(Qt::MetaModifier);
    const bool ctrl = event->modifiers().testFlag(Qt::ControlModifier);
    const bool shift = event->modifiers().testFlag(Qt::ShiftModifier);
    if (event->key() == Qt::Key_Escape ||
        (meta && event->key() == Qt::Key_A) ||
        (ctrl && shift && event->key() == Qt::Key_A)) {
        closeSidebar();
        event->accept();
        return;
    }
    QFrame::keyPressEvent(event);
}

void ActionCenterSidebar::refreshWindows()
{
    if (!windowsLayout_) {
        return;
    }
    QLayoutItem *item = nullptr;
    while ((item = windowsLayout_->takeAt(0)) != nullptr) {
        delete item->widget();
        delete item;
    }

    const QString rowStyle = QStringLiteral(
        "QPushButton { background: transparent; color: black; border: 1px solid transparent; "
        "text-align: left; padding: 6px 10px; font-size: 13px; }"
        "QPushButton:hover { background: %1; border: 1px solid %2; color: black; }"
        "QPushButton:pressed { background: %1; border: 2px solid %3; color: black; }")
                                 .arg(taskbar::hoverBackgroundColor().name(),
                                      taskbar::hoverBorderColor().name(),
                                      taskbar::pressedBorderColor().name());

    for (BrowserWindow *window : session_.browserWindows()) {
        if (!window) {
            continue;
        }
        auto *button = new QPushButton(this);
        button->setIcon(QIcon(window->taskbarIconPath()));
        button->setIconSize(QSize(20, 20));
        button->setText(QStringLiteral("  ") + window->taskbarTitle());
        button->setCursor(Qt::PointingHandCursor);
        button->setFocusPolicy(Qt::NoFocus);
        button->setFixedHeight(38);
        button->setStyleSheet(rowStyle);
        connect(button, &QPushButton::clicked, this, [this, window] {
            closeSidebar();
            session_.activateWindow(window);
        });
        windowsLayout_->addWidget(button);
    }
}

void ActionCenterSidebar::refreshDynamicTiles()
{
    if (keyboardTile_ && keyboardController_) {
        const auto &state = keyboardController_->state();
        const QString text = !state.currentLongName.isEmpty()
            ? state.currentLongName
            : (!state.currentShortName.isEmpty() ? state.currentShortName.toUpper() : QStringLiteral("Keyboard"));
        keyboardTile_->setTileText(text);
    }

    if (networkTile_) {
        if (auto *network = session_.networkController()) {
            const auto &state = network->state();
            networkTile_->setTileIcon(QIcon(iconPathForNetwork(state)));
            if (!state.available) {
                networkTile_->setTileText(QStringLiteral("No network"));
            } else if (state.type == seb::shell::taskbar::platform::NetworkState::Type::Wired) {
                networkTile_->setTileText(QStringLiteral("Wired"));
            } else if (state.status == seb::shell::taskbar::platform::NetworkState::Status::Connected) {
                QString activeSsid;
                for (const auto &net : state.networks) {
                    if (net.active) {
                        activeSsid = net.ssid;
                        break;
                    }
                }
                networkTile_->setTileText(activeSsid.isEmpty() ? QStringLiteral("Wi-Fi") : activeSsid);
            } else {
                networkTile_->setTileText(QStringLiteral("Disconnected"));
            }
        }
    }

    if (audioTile_ && audioController_) {
        const auto &state = audioController_->state();
        audioTile_->setTileIcon(QIcon(iconPathForAudio(state)));
        audioTile_->setTileText(!state.available
                                    ? QStringLiteral("No audio")
                                    : (state.muted
                                           ? QStringLiteral("Muted")
                                           : QStringLiteral("Audio (%1%)").arg(state.volumePercent)));
    }

    if (batteryTile_) {
        if (auto *battery = session_.batteryController()) {
            const auto &state = battery->state();
            batteryTile_->setTileIcon(buildBatteryTileIcon(state));
            if (state.charging) {
                batteryTile_->setTileText(QStringLiteral("Plugged in,\ncharging (%1%)").arg(state.percentage));
            } else if (state.powerConnected) {
                batteryTile_->setTileText(QStringLiteral("Plugged in\n(%1%)").arg(state.percentage));
            } else {
                batteryTile_->setTileText(QStringLiteral("Battery\n(%1%)").arg(state.percentage));
            }
        }
    }
}

void ActionCenterSidebar::refreshClock()
{
    if (!clockTile_) {
        return;
    }
    const QDateTime now = QDateTime::currentDateTime();
    clockTile_->setTileText(QStringLiteral("%1\n%2")
                                .arg(now.toString(QStringLiteral("HH:mm:ss")),
                                     now.toString(QStringLiteral("dd/MM/yyyy"))));
}

void ActionCenterSidebar::showAboutDialog()
{
    closeSidebar();
    const auto &appConfig = session_.configuration().appConfig;
    QMessageBox::information(
        window(),
        QStringLiteral("About Safe Exam Browser"),
        QStringLiteral("Safe Exam Browser for Linux\nVersion: %1\nSEB Compatibility: %2")
            .arg(appConfig.programBuildVersion, appConfig.sebCompatibilityVersion));
}

}  // namespace seb::ui
