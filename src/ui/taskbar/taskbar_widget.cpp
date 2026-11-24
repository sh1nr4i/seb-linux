#include "taskbar_widget.h"

#include "common/taskbar_button.h"
#include "common/taskbar_style.h"
#include "controls/application_control.h"
#include "controls/audio_control.h"
#include "controls/battery_control.h"
#include "controls/clock_widget.h"
#include "controls/external_application_control.h"
#include "controls/keyboard_control.h"
#include "controls/network_control.h"
#include "controls/notification_control.h"
#include "controls/raise_hand_control.h"

#include "../../shell/taskbar/platform/audio_controller.h"
#include "../../shell/taskbar/platform/battery_controller.h"
#include "../../shell/taskbar/platform/keyboard_controller.h"
#include "../../shell/taskbar/platform/network_controller.h"
#include "../../applications/external_application.h"
#include "../../shell/taskbar/taskbar_service.h"
#include "../../proctoring/screen_proctoring_service.h"
#include "../../seb_session.h"

#include <QFrame>
#include <QHBoxLayout>
#include <QScrollArea>

namespace seb::ui::taskbar {

TaskbarWidget::TaskbarWidget(SebSession &session, QWidget *parent)
    : QWidget(parent)
    , session_(session)
{
    setFixedHeight(kTaskbarHeight);
    setAutoFillBackground(true);
    setAttribute(Qt::WA_StyledBackground, true);
    QPalette lightPalette = palette();
    lightPalette.setColor(QPalette::Window, backgroundColor());
    lightPalette.setColor(QPalette::Base, backgroundColor());
    lightPalette.setColor(QPalette::Button, backgroundColor());
    lightPalette.setColor(QPalette::WindowText, Qt::black);
    lightPalette.setColor(QPalette::Text, Qt::black);
    lightPalette.setColor(QPalette::ButtonText, Qt::black);
    setPalette(lightPalette);
    setStyleSheet(QStringLiteral(
        "QWidget { background: %1; color: black; }"
        "QLabel { background: transparent; color: black; }")
                      .arg(backgroundColor().name()));

    auto *layout = new QHBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);

    auto *scrollArea = new QScrollArea(this);
    scrollArea->setFrameShape(QFrame::NoFrame);
    scrollArea->setWidgetResizable(true);
    scrollArea->setHorizontalScrollBarPolicy(Qt::ScrollBarAsNeeded);
    scrollArea->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    scrollArea->setFixedHeight(kTaskbarHeight);
    scrollArea->setStyleSheet(
        "QScrollArea { background: #f0f0f0; border: 0; }"
        "QScrollBar:horizontal { background: #d8d8d8; height: 8px; margin: 0; }"
        "QScrollBar::handle:horizontal { background: #b0b0b0; min-width: 20px; }"
        "QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal { width: 0; }");
    layout->addWidget(scrollArea, 1);

    auto *applicationContainer = new QWidget(scrollArea);
    auto *applicationLayout = new QHBoxLayout(applicationContainer);
    applicationLayout->setContentsMargins(0, 0, 0, 0);
    applicationLayout->setSpacing(0);
    applicationLayout->addWidget(new ApplicationControl(session_, applicationContainer));
    for (auto *application : session_.externalApplications()) {
        if (application->showInShell()) {
            applicationLayout->addWidget(new ExternalApplicationControl(*application, applicationContainer));
        }
    }
    applicationLayout->addStretch(1);
    scrollArea->setWidget(applicationContainer);

    audioController_ = new seb::shell::taskbar::platform::AudioController(this);
    networkController_ = session_.networkController();
    batteryController_ = session_.batteryController();
    keyboardController_ = new seb::shell::taskbar::platform::KeyboardController(this);

    const auto &settings = session_.settings();
    const auto model = seb::shell::taskbar::TaskbarService::buildModel(
        settings.UserInterface.Taskbar,
        settings.Security,
        settings.Proctoring,
        batteryController_->state().available);

    auto *notificationContainer = new QWidget(this);
    auto *notificationLayout = new QHBoxLayout(notificationContainer);
    notificationLayout->setContentsMargins(0, 0, 0, 0);
    notificationLayout->setSpacing(0);
    if (model.showApplicationInfo) {
        notificationLayout->addWidget(new NotificationControl(
            QStringLiteral(":/assets/taskbar/about.svg"),
            QStringLiteral("Application information"),
            false,
            notificationContainer));
    }
    if (model.showApplicationLog) {
        notificationLayout->addWidget(new NotificationControl(
            QStringLiteral(":/assets/taskbar/log.svg"),
            QStringLiteral("Application log"),
            false,
            notificationContainer));
    }
    if (model.showVerificator) {
        notificationLayout->addWidget(new NotificationControl(
            QStringLiteral(":/assets/taskbar/verificator.svg"),
            QStringLiteral("SEB Verificator"),
            false,
            notificationContainer));
    }
    if (model.showProctoring) {
        proctoringControl_ = new NotificationControl(
            QStringLiteral(":/assets/taskbar/proctoring.svg"),
            QStringLiteral("Screen Proctoring (Window only): Inactive"),
            false,
            true,
            notificationContainer);
        notificationLayout->addWidget(proctoringControl_);
        if (auto *sp = session_.screenProctoring()) {
            auto updateProctoringIndicator = [this, sp]() {
                if (!proctoringControl_) {
                    return;
                }
                const bool visible =
                    sp->state() != seb::proctoring::ScreenProctoringService::State::Inactive;
                proctoringControl_->setVisible(visible);
                proctoringControl_->setActive(sp->isActive());
                proctoringControl_->setToolTipText(sp->statusText());
            };
            connect(sp, &seb::proctoring::ScreenProctoringService::stateChanged,
                    this, [updateProctoringIndicator](seb::proctoring::ScreenProctoringService::State, const QString &) {
                        updateProctoringIndicator();
                    });
            updateProctoringIndicator();
        } else {
            proctoringControl_->setVisible(false);
        }
    }
    if (session_.isServerSession() && settings.Server.Invigilation.ShowRaiseHandNotification) {
        raiseHandControl_ = new RaiseHandControl(notificationContainer);
        raiseHandControl_->setMessageRequired(
            settings.Server.Invigilation.ForceRaiseHandMessage);
        raiseHandControl_->setServerCallbacks(
            [this](const QString &message, auto done) { session_.raiseServerHand(message, done); },
            [this](auto done) { session_.lowerServerHand(done); });
        connect(&session_, &SebSession::serverHandConfirmed,
                raiseHandControl_, &RaiseHandControl::resetRaisedState);
        notificationLayout->addWidget(raiseHandControl_);
    }
    layout->addWidget(notificationContainer);

    auto *systemContainer = new QWidget(this);
    auto *systemLayout = new QHBoxLayout(systemContainer);
    systemLayout->setContentsMargins(0, 0, 0, 0);
    systemLayout->setSpacing(0);
    systemLayout->addWidget(new BatteryControl(*batteryController_, systemContainer));
    if (model.showNetwork) {
        systemLayout->addWidget(new NetworkControl(*networkController_, systemContainer));
    }
    if (model.showAudio) {
        systemLayout->addWidget(new AudioControl(*audioController_, systemContainer));
    }
    if (model.showKeyboardLayout) {
        systemLayout->addWidget(new KeyboardControl(*keyboardController_, systemContainer));
    }
    layout->addWidget(systemContainer);

    if (model.showClock) {
        layout->addWidget(new ClockWidget(this));
    }

    if (model.showQuit) {
        auto *quitButton = new TaskbarButton(this);
        quitButton->setFixedWidth(40);
        quitButton->setToolTip(QStringLiteral("Quit Safe Exam Browser"));
        quitButton->setIconPath(QStringLiteral(":/assets/taskbar/shutdown.svg"));
        connect(quitButton, &QPushButton::clicked, this, &TaskbarWidget::quitRequested);
        layout->addWidget(quitButton);
    }
}

}  // namespace seb::ui::taskbar
