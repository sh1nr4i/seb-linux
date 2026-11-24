#include "battery_control.h"

#include "../common/taskbar_button.h"
#include "../common/taskbar_popup.h"
#include "../common/taskbar_style.h"
#include "../../../shell/taskbar/platform/battery_controller.h"

#include <QHBoxLayout>
#include <QIcon>
#include <QLabel>
#include <QPainter>
#include <QPixmap>
#include <QPushButton>
#include <QVBoxLayout>

namespace seb::ui::taskbar {

namespace {

QIcon buildBatteryIcon(const seb::shell::taskbar::platform::BatteryState &state)
{
    constexpr int size = 44;
    QPixmap pixmap(size, size);
    pixmap.fill(Qt::transparent);

    QPainter painter(&pixmap);
    painter.setRenderHint(QPainter::Antialiasing, true);

    const QRectF body(4.0, 12.0, 32.0, 20.0);
    const QRectF terminal(36.0, 17.0, 4.0, 10.0);

    // Subtle light interior so the battery stands out clearly against the taskbar
    painter.setPen(Qt::NoPen);
    painter.setBrush(QColor(QStringLiteral("#ffffff")));
    painter.drawRoundedRect(body, 3.0, 3.0);

    // Dynamic charge level fill
    const int clamped = qBound(0, state.percentage, 100);
    if (state.available && clamped > 0) {
        QColor fillColor(QStringLiteral("#2e7d32"));
        if (state.charging) {
            fillColor = QColor(QStringLiteral("#1e88e5"));
        } else if (clamped <= 15) {
            fillColor = QColor(QStringLiteral("#d32f2f"));
        } else if (clamped <= 30) {
            fillColor = QColor(QStringLiteral("#ed6c02"));
        }
        const double innerWidth = (body.width() - 6.0) * (clamped / 100.0);
        const QRectF fillRect(body.left() + 3.0, body.top() + 3.0, innerWidth, body.height() - 6.0);
        painter.setBrush(fillColor);
        painter.drawRoundedRect(fillRect, 1.5, 1.5);
    }

    // Crisp battery outline and terminal
    painter.setPen(QPen(QColor(QStringLiteral("#1f1f1f")), 2.6, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    painter.setBrush(Qt::NoBrush);
    painter.drawRoundedRect(body, 3.0, 3.0);

    painter.setPen(Qt::NoPen);
    painter.setBrush(QColor(QStringLiteral("#1f1f1f")));
    painter.drawRoundedRect(terminal, 1.5, 1.5);

    if (state.charging) {
        QPolygonF bolt;
        bolt << QPointF(21.5, 14.5)
             << QPointF(16.0, 22.5)
             << QPointF(20.0, 22.5)
             << QPointF(18.5, 29.5)
             << QPointF(24.5, 21.0)
             << QPointF(20.5, 21.0);
        painter.setPen(QPen(QColor(QStringLiteral("#1f1f1f")), 1.2));
        painter.setBrush(QColor(QStringLiteral("#ffd54f")));
        painter.drawPolygon(bolt);
    }

    return QIcon(pixmap);
}

}  // namespace

BatteryControl::BatteryControl(seb::shell::taskbar::platform::BatteryController &controller, QWidget *parent)
    : QWidget(parent)
    , controller_(controller)
{
    setFixedHeight(kTaskbarHeight);
    setAttribute(Qt::WA_StyledBackground, true);
    setStyleSheet(QStringLiteral("background: %1; color: black;").arg(backgroundColor().name()));

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);

    button_ = new TaskbarButton(this);
    button_->setFixedSize(40, kTaskbarHeight);
    layout->addWidget(button_);

    popup_ = new TaskbarPopup(this);
    auto *popupLayout = new QVBoxLayout(popup_);
    popupLayout->setContentsMargins(16, 10, 16, 16);
    popupLayout->setSpacing(8);

    auto *topRow = new QHBoxLayout();
    topRow->addStretch(1);
    auto *closeButton = new QPushButton(QStringLiteral("X"), popup_);
    closeButton->setFixedSize(22, 22);
    closeButton->setFlat(true);
    closeButton->setStyleSheet(QStringLiteral(
        "QPushButton { background: transparent; color: black; border: 1px solid transparent; font-weight: 700; }"
        "QPushButton:hover { background: %1; border: 1px solid %2; color: black; }"
        "QPushButton:pressed { background: %1; border: 2px solid %3; color: black; }")
                                   .arg(hoverBackgroundColor().name(),
                                        hoverBorderColor().name(),
                                        pressedBorderColor().name()));
    closeButton->setCursor(Qt::PointingHandCursor);
    topRow->addWidget(closeButton);
    popupLayout->addLayout(topRow);

    auto *textLabel = new QLabel(popup_);
    textLabel->setWordWrap(true);
    textLabel->setStyleSheet(QStringLiteral("background: transparent; color: black;"));
    popupLayout->addWidget(textLabel);
    connect(closeButton, &QPushButton::clicked, popup_, &QWidget::hide);

    connect(button_, &QPushButton::clicked, this, [this] {
        button_->setHasPopupOpen(true);
        popup_->showAbove(button_);
    });
    connect(&controller_, &seb::shell::taskbar::platform::BatteryController::stateChanged, this, &BatteryControl::updateState);
    connect(popup_, &TaskbarPopup::popupHidden, this, [this] {
        button_->setHasPopupOpen(false);
    });

    updateState();
}

void BatteryControl::updateState()
{
    const auto &state = controller_.state();
    QString tooltip = QStringLiteral("Battery unavailable");
    if (state.available) {
        tooltip = state.charging
            ? QStringLiteral("Charging: %1%").arg(state.percentage)
            : QStringLiteral("Battery remaining: %1%").arg(state.percentage);
        if (!state.timeRemaining.isEmpty()) {
            tooltip += QStringLiteral(" (%1)").arg(state.timeRemaining);
        }
    }

    setVisible(state.available);
    if (!state.available) popup_->hide();
    button_->setEnabled(state.available);
    button_->setToolTip(tooltip);
    button_->setCustomIcon(buildBatteryIcon(state));
    if (auto *label = popup_->findChild<QLabel *>()) {
        label->setText(tooltip);
    }
}

}  // namespace seb::ui::taskbar
