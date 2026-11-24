#pragma once

#include <QFrame>

QT_BEGIN_NAMESPACE
class QGridLayout;
class QHideEvent;
class QKeyEvent;
class QLabel;
class QTimer;
class QVBoxLayout;
QT_END_NAMESPACE

namespace seb::shell::taskbar::platform {
class AudioController;
class KeyboardController;
}

class SebSession;

namespace seb::ui {

class ActionCenterTile;

class ActionCenterSidebar : public QFrame
{
    Q_OBJECT

public:
    explicit ActionCenterSidebar(SebSession &session, QWidget *parent = nullptr);

    void toggleSidebar();
    void openSidebar();
    void closeSidebar();

signals:
    void quitRequested();

protected:
    void keyPressEvent(QKeyEvent *event) override;
    void hideEvent(QHideEvent *event) override;

private:
    void rebuildShortcutGrid();
    void refreshWindows();
    void refreshDynamicTiles();
    void refreshClock();
    void showAboutDialog();

    SebSession &session_;
    seb::shell::taskbar::platform::AudioController *audioController_ = nullptr;
    seb::shell::taskbar::platform::KeyboardController *keyboardController_ = nullptr;
    QVBoxLayout *windowsLayout_ = nullptr;
    QGridLayout *shortcutsGrid_ = nullptr;
    ActionCenterTile *keyboardTile_ = nullptr;
    ActionCenterTile *networkTile_ = nullptr;
    ActionCenterTile *audioTile_ = nullptr;
    ActionCenterTile *batteryTile_ = nullptr;
    ActionCenterTile *clockTile_ = nullptr;
    QTimer *clockTimer_ = nullptr;
};

}  // namespace seb::ui
