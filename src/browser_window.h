#pragma once

#include "settings/Browser/WindowSettings.h"
#include "browser/contracts/i_webview.h"

#include <QElapsedTimer>
#include <QPixmap>
#include <QPoint>
#include <QRect>
#include <QWidget>

#include <array>
#include <memory>

QT_BEGIN_NAMESPACE
class QAction;
class QAuthenticator;
class QCloseEvent;
class QEvent;
class QFocusEvent;
class QFrame;
class QKeyEvent;
class QLabel;
class QLineEdit;
class QPushButton;
class QResizeEvent;
class QToolBar;
class QToolButton;
class QUrl;
#if SEB_HAS_QTWEBENGINE
class QWebEngineNewWindowRequest;
class QWebEngineView;
#endif
QT_END_NAMESPACE

namespace seb::browser::contracts {
class IWebView;
}

namespace seb::ui {
class ActionCenterSidebar;
class WindowSwitcherOverlay;
}

class SebSession;
class SebTaskbar;

class BrowserWindow : public QWidget
{
    Q_OBJECT

public:
    BrowserWindow(
        SebSession &session,
        const seb::settings::browser::WindowSettings &windowSettings,
        const QUrl &initialUrl,
        bool isMainWindow,
        QWidget *parent = nullptr);
    ~BrowserWindow() override;

    QUrl currentUrl() const;
    bool isMainWindow() const;
    bool shouldAllowNavigation(const QUrl &url);
    QString taskbarIconPath() const;
    QString taskbarTitle() const;
    QWidget *workspaceWidget() const;
    void bringMainContentToFront();
    void setLockdownActive(bool active);
    bool isLockdownActive() const { return lockdownActive_; }
    QPixmap previewPixmap(const QSize &targetSize) const;
    void stepWindowSwitcher(bool backwards = false);
    void commitWindowSwitcher();
    void cancelWindowSwitcher();
    void toggleActionCenter();
    void showSnapPreview(const QRect &targetRect, QWidget *draggedWindow);
    void hideSnapPreview();

protected:
    void changeEvent(QEvent *event) override;
    void closeEvent(QCloseEvent *event) override;
    void keyPressEvent(QKeyEvent *event) override;
    void focusInEvent(QFocusEvent *event) override;
    void focusOutEvent(QFocusEvent *event) override;
    void resizeEvent(QResizeEvent *event) override;
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    QWidget *managedWindowFrame() const;
    QRect workspaceBounds() const;
    int hitTestResizeEdges(const QPoint &localPos, const QSize &size) const;
    void updateCursorForEdges(QWidget *target, int edges);
    QRect computeSnapRect(const QPoint &workspacePos, bool &isMaximizeSnap) const;
    void updateFrameMargins();
    void setupResizeHandles(QWidget *frame);
    void layoutResizeHandles();
    bool isResizeHandle(QObject *watched) const;
    QFrame *createTitleBar(QWidget *parent);
    void applyWindowFlags();
    void applyWindowGeometry();
    void configureToolbar(QWidget *toolbarHost);
    void configureSidebar();
    void configureShortcuts();
    void toggleZoomPopup();
    void toggleFindPopup();
    void findInPage();
    void navigateHome();
    void requestQuit(bool confirmWithoutPassword = true);
    void minimizeManagedWindow();
    void toggleManagedWindowMaximized();
    void handleNewWindowRequest(const QUrl &url, seb::browser::contracts::IWebView *&targetView);
    void handleBrowserCommand(seb::browser::contracts::IWebView::Command command);
    void changeZoom(int steps);
    void reloadPage();
    void updateAddressBar(const QUrl &url);
    void updateToolbarState();
    void notifyTaskbarStateChanged();

    SebSession &session_;
    seb::settings::browser::WindowSettings windowSettings_;
    std::unique_ptr<seb::browser::contracts::IWebView> view_;
    QWidget *workspace_ = nullptr;
    QFrame *snapPreview_ = nullptr;
    QFrame *windowFrame_ = nullptr;
    std::array<QWidget *, 4> resizeHandles_{{nullptr, nullptr, nullptr, nullptr}};
    QFrame *titleBar_ = nullptr;
    QToolBar *toolbar_ = nullptr;
    QAction *backAction_ = nullptr;
    QAction *forwardAction_ = nullptr;
    QAction *reloadAction_ = nullptr;
    QAction *homeAction_ = nullptr;
    QLineEdit *addressBar_ = nullptr;
    QLabel *titleLabel_ = nullptr;
    QToolButton *zoomMenuButton_ = nullptr;
    QFrame *zoomPopup_ = nullptr;
    QPushButton *zoomLevelButton_ = nullptr;
    QToolButton *findMenuButton_ = nullptr;
    QFrame *findPopup_ = nullptr;
    QLineEdit *findLineEdit_ = nullptr;
    QPushButton *minimizeButton_ = nullptr;
    QPushButton *maximizeButton_ = nullptr;
    SebTaskbar *taskbar_ = nullptr;
    seb::ui::ActionCenterSidebar *sidebar_ = nullptr;
    seb::ui::WindowSwitcherOverlay *windowSwitcher_ = nullptr;
    mutable QPixmap lastPreviewPixmap_;
    bool isMainWindow_ = false;
    bool lockdownActive_ = false;
    bool windowMaximized_ = true;
    bool windowSnapped_ = false;
    bool dragPending_ = false;
    bool draggingWindow_ = false;
    bool resizingWindow_ = false;
    int activeResizeEdges_ = 0;
    QPoint dragStartGlobalPos_;
    QPoint dragStartLocalPos_;
    QPoint dragOffset_;
    QPoint resizeStartGlobalPos_;
    QRect resizeStartGeometry_;
    QRect activeSnapRect_;
    bool activeSnapIsMaximize_ = false;
    QRect normalWindowGeometry_;
    bool reloadConfirmActive_ = false;
    bool quitPromptActive_ = false;
    QElapsedTimer sidebarToggleDebounce_;
    int zoomPercentage_ = 100;
};
