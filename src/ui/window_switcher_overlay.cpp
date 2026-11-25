#include "window_switcher_overlay.h"

#include "taskbar/common/taskbar_style.h"
#include "../browser_window.h"
#include "../seb_session.h"

#include <QGridLayout>
#include <QHBoxLayout>
#include <QLabel>
#include <QMouseEvent>
#include <QPixmap>
#include <QVBoxLayout>

#include <functional>
#include <utility>

namespace seb::ui {

namespace {

constexpr int kCardWidth = 224;
constexpr int kCardHeight = 160;
constexpr int kThumbWidth = 204;
constexpr int kThumbHeight = 114;
constexpr int kMaxColumns = 4;

class ClickableSwitcherCard : public QFrame
{
public:
    explicit ClickableSwitcherCard(std::function<void()> onClick, QWidget *parent = nullptr)
        : QFrame(parent)
        , onClick_(std::move(onClick))
    {
        setCursor(Qt::PointingHandCursor);
    }

protected:
    void mousePressEvent(QMouseEvent *event) override
    {
        if (event && event->button() == Qt::LeftButton && onClick_) {
            onClick_();
            event->accept();
            return;
        }
        QFrame::mousePressEvent(event);
    }

private:
    std::function<void()> onClick_;
};

}  // namespace

WindowSwitcherOverlay::WindowSwitcherOverlay(SebSession &session, QWidget *parent)
    : QFrame(parent)
    , session_(session)
{
    setObjectName(QStringLiteral("sebWindowSwitcherOverlay"));
    setAttribute(Qt::WA_StyledBackground, true);
    setAutoFillBackground(true);
    setFocusPolicy(Qt::NoFocus);

    setStyleSheet(QStringLiteral(
        "#sebWindowSwitcherOverlay { background: %1; border: 1px solid gray; border-radius: 8px; }")
                      .arg(taskbar::backgroundColor().name()));

    auto *outerLayout = new QVBoxLayout(this);
    outerLayout->setContentsMargins(14, 14, 14, 14);
    outerLayout->setSpacing(0);

    cardsGrid_ = new QGridLayout();
    cardsGrid_->setContentsMargins(0, 0, 0, 0);
    cardsGrid_->setHorizontalSpacing(10);
    cardsGrid_->setVerticalSpacing(10);
    outerLayout->addLayout(cardsGrid_);

    hide();
}

void WindowSwitcherOverlay::step(bool backwards)
{
    if (!isVisible()) {
        rebuildCards();
        if (entries_.isEmpty()) {
            return;
        }
        if (entries_.size() == 1) {
            selectedIndex_ = 0;
        } else {
            int activeIdx = 0;
            for (int i = 0; i < entries_.size(); ++i) {
                if (entries_[i].window && entries_[i].window->isActiveWindow()) {
                    activeIdx = i;
                    break;
                }
            }
            const int count = static_cast<int>(entries_.size());
            selectedIndex_ = backwards
                ? (activeIdx + count - 1) % count
                : (activeIdx + 1) % count;
        }
        updateSelectionStyles();
        adjustSize();
        repositionInParent();
        show();
        raise();
        return;
    }

    if (entries_.isEmpty()) {
        hide();
        return;
    }

    const int count = static_cast<int>(entries_.size());
    selectedIndex_ = backwards
        ? (selectedIndex_ + count - 1) % count
        : (selectedIndex_ + 1) % count;
    updateSelectionStyles();
    raise();
}

void WindowSwitcherOverlay::commit()
{
    if (!isVisible()) {
        return;
    }
    const int index = selectedIndex_;
    hide();
    activateEntry(index);
}

void WindowSwitcherOverlay::cancel()
{
    if (!isVisible()) {
        return;
    }
    hide();
}

void WindowSwitcherOverlay::repositionInParent()
{
    if (!parentWidget()) {
        return;
    }
    const QRect bounds = parentWidget()->rect();
    const QSize sz = sizeHint();
    const int w = qMin(sz.width(), qMax(240, bounds.width() - 32));
    const int h = qMin(sz.height(), qMax(180, bounds.height() - 32));
    resize(w, h);
    move(bounds.left() + (bounds.width() - w) / 2,
         bounds.top() + (bounds.height() - h) / 2);
}

void WindowSwitcherOverlay::rebuildCards()
{
    QLayoutItem *item = nullptr;
    while ((item = cardsGrid_->takeAt(0)) != nullptr) {
        delete item->widget();
        delete item;
    }
    entries_.clear();

    const QList<BrowserWindow *> windows = session_.browserWindows();
    int index = 0;
    for (BrowserWindow *window : windows) {
        if (!window) {
            continue;
        }

        const int cardIndex = index;
        auto *card = new ClickableSwitcherCard([this, cardIndex] {
            selectedIndex_ = cardIndex;
            commit();
        }, this);
        card->setObjectName(QStringLiteral("sebSwitcherCard"));
        card->setFixedSize(kCardWidth, kCardHeight);
        card->setAttribute(Qt::WA_StyledBackground, true);

        auto *cardLayout = new QVBoxLayout(card);
        cardLayout->setContentsMargins(8, 7, 8, 8);
        cardLayout->setSpacing(6);

        auto *headerRow = new QHBoxLayout();
        headerRow->setContentsMargins(0, 0, 0, 0);
        headerRow->setSpacing(6);

        auto *iconLabel = new QLabel(card);
        iconLabel->setPixmap(QPixmap(window->taskbarIconPath())
                                 .scaled(14, 14, Qt::KeepAspectRatio, Qt::SmoothTransformation));
        iconLabel->setFixedSize(14, 14);
        iconLabel->setStyleSheet(QStringLiteral("background: transparent; border: none;"));
        headerRow->addWidget(iconLabel);

        const QString rawTitle = window->taskbarTitle();
        const QString elidedTitle = rawTitle.size() > 26
            ? rawTitle.left(23) + QStringLiteral("...")
            : rawTitle;
        auto *titleLabel = new QLabel(elidedTitle, card);
        titleLabel->setStyleSheet(QStringLiteral(
            "background: transparent; border: none; color: black; font-size: 11px; font-weight: 600;"));
        headerRow->addWidget(titleLabel, 1);
        cardLayout->addLayout(headerRow);

        auto *thumbFrame = new QLabel(card);
        thumbFrame->setFixedSize(kThumbWidth, kThumbHeight);
        thumbFrame->setAlignment(Qt::AlignCenter);
        thumbFrame->setStyleSheet(QStringLiteral(
            "background: #d8d8d8; border: 1px solid #b0b0b0; color: #696969; font-size: 11px;"));

        const QPixmap preview = window->previewPixmap(QSize(kThumbWidth - 2, kThumbHeight - 2));
        if (!preview.isNull()) {
            thumbFrame->setPixmap(preview);
        } else {
            thumbFrame->setText(QStringLiteral("Safe Exam Browser"));
        }
        cardLayout->addWidget(thumbFrame, 1, Qt::AlignCenter);

        cardsGrid_->addWidget(card, index / kMaxColumns, index % kMaxColumns);
        entries_.append(CardEntry{window, card});
        ++index;
    }
}

void WindowSwitcherOverlay::updateSelectionStyles()
{
    const QString selectedStyle = QStringLiteral(
        "#sebSwitcherCard { background: %1; border: 2px solid %2; border-radius: 4px; }")
                                      .arg(taskbar::hoverBackgroundColor().name(),
                                           taskbar::hoverBorderColor().name());
    const QString normalStyle = QStringLiteral(
        "#sebSwitcherCard { background: lightgray; border: 1px solid gray; border-radius: 4px; }"
        "#sebSwitcherCard:hover { background: %1; border: 1px solid %2; }")
                                    .arg(taskbar::hoverBackgroundColor().name(),
                                         taskbar::hoverBorderColor().name());

    for (int i = 0; i < entries_.size(); ++i) {
        if (entries_[i].card) {
            entries_[i].card->setStyleSheet(i == selectedIndex_ ? selectedStyle : normalStyle);
        }
    }
}

void WindowSwitcherOverlay::activateEntry(int index)
{
    if (index < 0 || index >= entries_.size()) {
        return;
    }
    if (BrowserWindow *target = entries_[index].window) {
        session_.activateWindow(target);
    }
}

}  // namespace seb::ui
