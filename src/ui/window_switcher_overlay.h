#pragma once

#include <QFrame>
#include <QList>
#include <QPointer>

QT_BEGIN_NAMESPACE
class QGridLayout;
QT_END_NAMESPACE

class BrowserWindow;
class SebSession;

namespace seb::ui {

class WindowSwitcherOverlay : public QFrame
{
    Q_OBJECT

public:
    explicit WindowSwitcherOverlay(SebSession &session, QWidget *parent = nullptr);

    void step(bool backwards = false);
    void commit();
    void cancel();
    void repositionInParent();

private:
    struct CardEntry
    {
        QPointer<BrowserWindow> window;
        QFrame *card = nullptr;
    };

    void rebuildCards();
    void updateSelectionStyles();
    void activateEntry(int index);

    SebSession &session_;
    QGridLayout *cardsGrid_ = nullptr;
    QList<CardEntry> entries_;
    int selectedIndex_ = 0;
};

}  // namespace seb::ui
