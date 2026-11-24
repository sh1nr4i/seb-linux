#include "browser_window.h"

#include "seb_session.h"
#include "ui/action_center_sidebar.h"
#include "ui/seb_taskbar.h"
#include "ui/taskbar/common/taskbar_style.h"

#include "browser/configuration_resource_detector.h"
#include "browser/contracts/i_engine_provider.h"
#include "browser/contracts/i_webprofile.h"
#include "browser/contracts/i_webview.h"

#include <QAction>
#include <QApplication>
#include <QAuthenticator>
#include <QCloseEvent>
#include <QComboBox>
#include <QDir>
#include <QEvent>
#include <QFileDialog>
#include <QFileInfo>
#include <QFocusEvent>
#include <QFrame>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QIcon>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QMouseEvent>
#include <QPainter>
#include <QPainterPath>
#include <QPalette>
#include <QPixmap>
#include <QPushButton>
#include <QRegion>
#include <QResizeEvent>
#include <QScreen>
#include <QSize>
#include <QToolBar>
#include <QToolButton>
#include <QUrl>
#include <QVBoxLayout>

#include <algorithm>
#include <limits>

namespace {

constexpr int kTitleBarHeight = 26;
constexpr int kResizeBorder = 6;
constexpr int kWindowCornerRadius = 8;
constexpr int kMinWindowWidth = 360;
constexpr int kMinWindowHeight = 240;
constexpr int kSnapEdgeThreshold = 18;
constexpr int kSnapCornerThreshold = 64;

enum ResizeEdge : int {
    ResizeNone = 0,
    ResizeLeft = 1 << 0,
    ResizeRight = 1 << 1,
    ResizeTop = 1 << 2,
    ResizeBottom = 1 << 3
};

void hideFileSystemElementPath(QFileDialog &dialog)
{
    if (auto *label = dialog.findChild<QLabel *>(QStringLiteral("lookInLabel"))) {
        label->hide();
    }
    if (auto *location = dialog.findChild<QComboBox *>(QStringLiteral("lookInCombo"))) {
        location->hide();
    }
}

void configureFileDialog(
    QFileDialog &dialog,
    bool customLocation,
    bool showElementPath)
{
    const bool useNonNativeDialog = !customLocation || !showElementPath;
    dialog.setOption(QFileDialog::DontUseNativeDialog, useNonNativeDialog);
    if (!showElementPath && useNonNativeDialog) {
        hideFileSystemElementPath(dialog);
    }
}

QIcon buildZoomIcon()
{
    QPixmap pixmap(44, 44);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(QPen(Qt::black, 3.0, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    p.setBrush(Qt::NoBrush);
    p.drawEllipse(QRectF(7, 7, 22, 22));
    p.drawLine(QPointF(26, 26), QPointF(37, 37));
    p.setPen(QPen(Qt::black, 2.4, Qt::SolidLine, Qt::RoundCap));
    p.drawLine(QPointF(18, 13), QPointF(18, 23));
    p.drawLine(QPointF(13, 18), QPointF(23, 18));
    return QIcon(pixmap);
}

QIcon buildSearchIcon()
{
    QPixmap pixmap(44, 44);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, true);
    p.setPen(QPen(Qt::black, 3.0, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    p.setBrush(Qt::NoBrush);
    p.drawEllipse(QRectF(8, 8, 20, 20));
    p.drawLine(QPointF(25, 25), QPointF(36, 36));
    return QIcon(pixmap);
}

QIcon buildMaximizeIcon()
{
    QPixmap pixmap(24, 24);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, false);
    p.setPen(QPen(Qt::black, 1.5));
    p.setBrush(Qt::NoBrush);
    p.drawRect(QRectF(5.5, 5.5, 13.0, 13.0));
    return QIcon(pixmap);
}

QIcon buildRestoreIcon()
{
    QPixmap pixmap(24, 24);
    pixmap.fill(Qt::transparent);
    QPainter p(&pixmap);
    p.setRenderHint(QPainter::Antialiasing, false);
    p.setPen(QPen(Qt::black, 1.5));
    p.setBrush(Qt::NoBrush);
    // Back square (top-right offset)
    p.drawLine(QPointF(8.5, 4.5), QPointF(19.5, 4.5));
    p.drawLine(QPointF(19.5, 4.5), QPointF(19.5, 15.5));
    p.drawLine(QPointF(19.5, 15.5), QPointF(16.5, 15.5));
    p.drawLine(QPointF(8.5, 4.5), QPointF(8.5, 7.5));
    // Front square (bottom-left)
    p.drawRect(QRectF(4.5, 8.5, 11.0, 11.0));
    return QIcon(pixmap);
}

QString taskbarButtonStyleSheet()
{
    return QStringLiteral(
               "QPushButton { background: %1; color: black; border: 1px solid %1; padding: 0; font-size: 13px; }"
               "QPushButton:hover { background: %2; border: 1px solid %3; color: black; }"
               "QPushButton:pressed { background: %2; border: 2px solid %4; color: black; }"
               "QPushButton:disabled { background: %1; color: rgba(0, 0, 0, 64); border: 1px solid %1; }")
        .arg(seb::ui::taskbar::backgroundColor().name(),
             seb::ui::taskbar::hoverBackgroundColor().name(),
             seb::ui::taskbar::hoverBorderColor().name(),
             seb::ui::taskbar::pressedBorderColor().name());
}

class HairlineDivider : public QWidget
{
public:
    explicit HairlineDivider(QWidget *parent = nullptr)
        : QWidget(parent)
    {
        setFixedHeight(1);
        setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    }

protected:
    void paintEvent(QPaintEvent *event) override
    {
        Q_UNUSED(event);
        QPainter painter(this);
        painter.fillRect(rect(), seb::ui::taskbar::backgroundColor());
        painter.fillRect(QRectF(0.0, 0.5, width(), 0.5), QColor(QStringLiteral("#b0b0b0")));
    }
};

QWidget *createDividerLine(QWidget *parent)
{
    return new HairlineDivider(parent);
}

}  // namespace

BrowserWindow::BrowserWindow(
    SebSession &session,
    const seb::settings::browser::WindowSettings &windowSettings,
    const QUrl &initialUrl,
    bool isMainWindow,
    QWidget *parent)
    : QWidget(
          parent,
          (!isMainWindow && parent)
              ? Qt::Widget
              : (Qt::Window | Qt::FramelessWindowHint | Qt::CustomizeWindowHint))
    , session_(session)
    , windowSettings_(windowSettings)
    , isMainWindow_(isMainWindow)
    , windowMaximized_(isMainWindow)
{
    applyWindowFlags();

    if (isMainWindow_) {
        auto *outerLayout = new QVBoxLayout(this);
        outerLayout->setContentsMargins(0, 0, 0, 0);
        outerLayout->setSpacing(0);

        workspace_ = new QWidget(this);
        workspace_->setStyleSheet(QStringLiteral("background: #d8d8d8;"));
        outerLayout->addWidget(workspace_, 1);

        snapPreview_ = new QFrame(workspace_);
        snapPreview_->setObjectName(QStringLiteral("sebSnapPreview"));
        snapPreview_->setAttribute(Qt::WA_TransparentForMouseEvents, true);
        snapPreview_->setStyleSheet(QStringLiteral(
            "#sebSnapPreview { background: rgba(173, 216, 230, 120); border: 1px solid %1; }")
                                        .arg(seb::ui::taskbar::hoverBorderColor().name()));
        snapPreview_->hide();

        windowFrame_ = new QFrame(workspace_);
        windowFrame_->setObjectName(QStringLiteral("sebInternalWindowFrame"));
        windowFrame_->setMouseTracking(true);
        windowFrame_->setStyleSheet(QStringLiteral(
            "#sebInternalWindowFrame { background: %1; border: none; }")
                                        .arg(seb::ui::taskbar::backgroundColor().name()));

        auto *frameLayout = new QVBoxLayout(windowFrame_);
        frameLayout->setContentsMargins(0, 0, 0, 0);
        frameLayout->setSpacing(0);

        titleBar_ = createTitleBar(windowFrame_);
        titleBar_->setMouseTracking(true);
        frameLayout->addWidget(titleBar_);

        configureToolbar(windowFrame_);
        if (toolbar_) {
            frameLayout->addWidget(toolbar_);
        }
        frameLayout->addWidget(createDividerLine(windowFrame_));

        view_ = session_.engineProvider()->createWebView(session_.profile(), windowFrame_);
        view_->configureBrowser(session_.settings(), windowSettings_);
        frameLayout->addWidget(view_->widget(), 1);
        setupResizeHandles(windowFrame_);

        if (session_.settings().UserInterface.Taskbar.EnableTaskbar) {
            taskbar_ = new SebTaskbar(session_, this);
            outerLayout->addWidget(taskbar_);
        }

        workspace_->installEventFilter(this);
        windowFrame_->installEventFilter(this);
    } else {
        setObjectName(QStringLiteral("sebSubBrowserWindow"));
        setAutoFillBackground(true);
        setAttribute(Qt::WA_StyledBackground, true);
        setMouseTracking(true);
        setStyleSheet(QStringLiteral(
            "#sebSubBrowserWindow { background: %1; border: none; }")
                          .arg(seb::ui::taskbar::backgroundColor().name()));

        auto *contentLayout = new QVBoxLayout(this);
        contentLayout->setContentsMargins(0, 0, 0, 0);
        contentLayout->setSpacing(0);

        titleBar_ = createTitleBar(this);
        titleBar_->setMouseTracking(true);
        contentLayout->addWidget(titleBar_);

        configureToolbar(this);
        if (toolbar_) {
            contentLayout->addWidget(toolbar_);
        }
        contentLayout->addWidget(createDividerLine(this));

        view_ = session_.engineProvider()->createWebView(session_.profile(), this);
        view_->configureBrowser(session_.settings(), windowSettings_);
        contentLayout->addWidget(view_->widget(), 1);
        setupResizeHandles(this);
        installEventFilter(this);
    }

    view_->initializeClipboard(session_.clipboard());
    view_->initializeSebApi(
        session_.keyGenerator(),
        session_.settings().Browser,
        session_.configuration().appConfig);

    view_->setNavigationRequestDelegate([this](const QUrl &url, bool isMainFrame) {
        if (isMainFrame && !this->shouldAllowNavigation(url)) {
            return false;
        }
        return true;
    });

    view_->setFileSelectionDelegate([this](int mode, const QStringList &oldFiles, const QStringList &acceptedMimeTypes) {
        const bool save = mode == 3;
        if (save && !session_.settings().Browser.AllowDownloads) {
            return QStringList();
        }
        if (!save && !session_.settings().Browser.AllowUploads) {
            return QStringList();
        }

        const QString baseDirectory = session_.downAndUploadDirectory();
        const bool customLocation = session_.settings().Browser.AllowCustomDownAndUploadLocation;
        const bool showPath = session_.settings().Browser.ShowFileSystemElementPath;
        const QString allowed = QDir(baseDirectory).canonicalPath();

        auto restrictDialogDirectory = [&](QFileDialog &dialog) {
            if (customLocation || allowed.isEmpty()) {
                return;
            }
            QObject::connect(
                &dialog,
                &QFileDialog::directoryEntered,
                &dialog,
                [&dialog, allowed](const QString &directory) {
                    const QString canonical = QFileInfo(directory).canonicalFilePath();
                    if (canonical != allowed &&
                        !canonical.startsWith(allowed + QDir::separator())) {
                        dialog.setDirectory(allowed);
                    }
                });
        };

        auto uniquePath = [](const QString &directory, const QString &name) {
            QFileInfo candidate(QDir(directory).filePath(name));
            if (!candidate.exists()) {
                return candidate.filePath();
            }

            const QString stem = candidate.completeBaseName();
            const QString suffix = candidate.completeSuffix();
            for (int index = 1; index < std::numeric_limits<int>::max(); ++index) {
                const QString numbered = suffix.isEmpty()
                    ? QStringLiteral("%1(%2)").arg(stem).arg(index)
                    : QStringLiteral("%1(%2).%3").arg(stem).arg(index).arg(suffix);
                candidate.setFile(QDir(directory).filePath(numbered));
                if (!candidate.exists()) {
                    return candidate.filePath();
                }
            }
            return QString();
        };

        QStringList selected;
        if (save) {
            if (customLocation) {
                QFileDialog dialog(this, QStringLiteral("Save Download"), baseDirectory);
                dialog.setAcceptMode(QFileDialog::AcceptSave);
                dialog.setFileMode(QFileDialog::AnyFile);
                configureFileDialog(dialog, customLocation, showPath);
                restrictDialogDirectory(dialog);
                if (dialog.exec() == QDialog::Accepted) {
                    selected = dialog.selectedFiles();
                }
            } else {
                const QString requestedName = oldFiles.value(0).trimmed();
                const QString name = QFileInfo(requestedName).fileName().isEmpty()
                    ? QStringLiteral("download")
                    : QFileInfo(requestedName).fileName();
                const QString file = uniquePath(baseDirectory, name);
                if (!file.isEmpty()) {
                    selected << file;
                }
            }
        } else if (mode == 2) {
            QFileDialog dialog(this, QStringLiteral("Select Upload Folder"), baseDirectory);
            dialog.setFileMode(QFileDialog::Directory);
            dialog.setOption(QFileDialog::ShowDirsOnly, true);
            configureFileDialog(dialog, customLocation, showPath);
            restrictDialogDirectory(dialog);
            if (dialog.exec() == QDialog::Accepted) {
                selected = dialog.selectedFiles();
            }
        } else if (mode == 1) {
            QFileDialog dialog(this, QStringLiteral("Select Files to Upload"), baseDirectory);
            dialog.setFileMode(QFileDialog::ExistingFiles);
            configureFileDialog(dialog, customLocation, showPath);
            restrictDialogDirectory(dialog);
            if (!acceptedMimeTypes.isEmpty()) {
                dialog.setMimeTypeFilters(acceptedMimeTypes);
            }
            if (dialog.exec() == QDialog::Accepted) {
                selected = dialog.selectedFiles();
            }
        } else {
            QFileDialog dialog(this, QStringLiteral("Select File to Upload"), baseDirectory);
            dialog.setFileMode(QFileDialog::ExistingFile);
            configureFileDialog(dialog, customLocation, showPath);
            restrictDialogDirectory(dialog);
            if (!acceptedMimeTypes.isEmpty()) {
                dialog.setMimeTypeFilters(acceptedMimeTypes);
            }
            if (dialog.exec() == QDialog::Accepted) {
                selected = dialog.selectedFiles();
            }
        }

        if (customLocation) {
            return selected;
        }

        if (allowed.isEmpty()) {
            return QStringList();
        }

        selected.erase(
            std::remove_if(selected.begin(), selected.end(), [&allowed, save](const QString &path) {
                const QFileInfo info(path);
                const QString canonical = save
                    ? info.dir().canonicalPath()
                    : info.canonicalFilePath();
                return canonical.isEmpty() ||
                       (canonical != allowed &&
                        !canonical.startsWith(allowed + QDir::separator()));
            }),
            selected.end());
        return selected;
    });

    if (isMainWindow_) {
        configureSidebar();
    }
    configureShortcuts();
    applyWindowGeometry();

    connect(view_.get(), &seb::browser::contracts::IWebView::urlChanged, this, &BrowserWindow::updateAddressBar);
    connect(view_.get(), &seb::browser::contracts::IWebView::titleChanged, this, [this](const QString &title) {
        const QString resolvedTitle = title.trimmed().isEmpty() ? QStringLiteral("Safe Exam Browser") : title;
        setWindowTitle(resolvedTitle);
        if (taskbar_) {
            taskbar_->setWindowTitleText(resolvedTitle);
        }
        updateToolbarState();
        notifyTaskbarStateChanged();
    });
    connect(view_.get(), &seb::browser::contracts::IWebView::newWindowRequested, this, &BrowserWindow::handleNewWindowRequest);
    connect(view_.get(), &seb::browser::contracts::IWebView::commandRequested, this, &BrowserWindow::handleBrowserCommand, Qt::QueuedConnection);
    connect(view_.get(), &seb::browser::contracts::IWebView::closeRequested, this, [this] {
        if (!isMainWindow_) close();
    }, Qt::QueuedConnection);
    connect(
        view_.get(),
        &seb::browser::contracts::IWebView::proxyAuthenticationRequired,
        this,
        [this](const QUrl &url, QAuthenticator *authenticator, const QString &proxyHost) {
            session_.applyProxyAuthentication(url, proxyHost, authenticator);
        });

    if (taskbar_) {
        connect(taskbar_, &SebTaskbar::quitRequested, this, [this] {
            if (isMainWindow_) {
                requestQuit(true);
            }
        });
    }

    if (initialUrl.isValid()) {
        view_->setUrl(initialUrl);
    }
    updateToolbarState();

    if (view_->widget()) {
        view_->widget()->installEventFilter(this);
    }
    if (titleBar_) {
        titleBar_->installEventFilter(this);
    }

    session_.registerBrowserWindow(this);
    notifyTaskbarStateChanged();
}

BrowserWindow::~BrowserWindow()
{
    session_.unregisterBrowserWindow(this);
}

QWidget *BrowserWindow::managedWindowFrame() const
{
    return isMainWindow_ ? static_cast<QWidget *>(windowFrame_) : const_cast<BrowserWindow *>(this);
}

QRect BrowserWindow::workspaceBounds() const
{
    if (isMainWindow_) {
        return workspace_ ? workspace_->rect() : rect();
    }
    return parentWidget() ? parentWidget()->rect() : rect();
}

void BrowserWindow::showSnapPreview(const QRect &targetRect, QWidget *draggedWindow)
{
    if (!isMainWindow_) {
        if (BrowserWindow *main = session_.mainWindow()) {
            main->showSnapPreview(targetRect, draggedWindow);
        }
        return;
    }
    if (!snapPreview_) {
        return;
    }
    if (!targetRect.isValid() || targetRect.isEmpty()) {
        snapPreview_->hide();
        return;
    }
    snapPreview_->setGeometry(targetRect);
    snapPreview_->show();
    snapPreview_->raise();
    Q_UNUSED(draggedWindow);
}

void BrowserWindow::hideSnapPreview()
{
    if (!isMainWindow_) {
        if (BrowserWindow *main = session_.mainWindow()) {
            main->hideSnapPreview();
        }
        return;
    }
    if (snapPreview_) {
        snapPreview_->hide();
    }
}

int BrowserWindow::hitTestResizeEdges(const QPoint &localPos, const QSize &size) const
{
    if (windowMaximized_) {
        return ResizeNone;
    }
    int edges = ResizeNone;
    if (localPos.x() <= kResizeBorder) edges |= ResizeLeft;
    if (localPos.x() >= size.width() - kResizeBorder) edges |= ResizeRight;
    if (localPos.y() <= kResizeBorder) edges |= ResizeTop;
    if (localPos.y() >= size.height() - kResizeBorder) edges |= ResizeBottom;
    return edges;
}

void BrowserWindow::updateCursorForEdges(QWidget *target, int edges)
{
    if (!target) {
        return;
    }
    if ((edges == (ResizeLeft | ResizeTop)) || (edges == (ResizeRight | ResizeBottom))) {
        target->setCursor(Qt::SizeFDiagCursor);
    } else if ((edges == (ResizeRight | ResizeTop)) || (edges == (ResizeLeft | ResizeBottom))) {
        target->setCursor(Qt::SizeBDiagCursor);
    } else if (edges & (ResizeLeft | ResizeRight)) {
        target->setCursor(Qt::SizeHorCursor);
    } else if (edges & (ResizeTop | ResizeBottom)) {
        target->setCursor(Qt::SizeVerCursor);
    } else {
        target->unsetCursor();
    }
}

QRect BrowserWindow::computeSnapRect(const QPoint &workspacePos, bool &isMaximizeSnap) const
{
    isMaximizeSnap = false;
    const QRect b = workspaceBounds();
    if (!b.isValid() || b.isEmpty()) {
        return {};
    }

    const int x = workspacePos.x();
    const int y = workspacePos.y();
    const int w = b.width();
    const int h = b.height();
    const int halfW = w / 2;
    const int halfH = h / 2;

    const bool nearLeft = x <= b.left() + kSnapEdgeThreshold;
    const bool nearRight = x >= b.right() - kSnapEdgeThreshold;
    const bool nearTop = y <= b.top() + kSnapEdgeThreshold;
    const bool nearBottom = y >= b.bottom() - kSnapEdgeThreshold;

    const bool cornerLeft = x <= b.left() + kSnapCornerThreshold;
    const bool cornerRight = x >= b.right() - kSnapCornerThreshold;
    const bool cornerTop = y <= b.top() + kSnapCornerThreshold;
    const bool cornerBottom = y >= b.bottom() - kSnapCornerThreshold;

    if ((nearLeft && cornerTop) || (nearTop && cornerLeft)) {
        return QRect(b.left(), b.top(), halfW, halfH);
    }
    if ((nearLeft && cornerBottom) || (nearBottom && cornerLeft)) {
        return QRect(b.left(), b.top() + halfH, halfW, h - halfH);
    }
    if ((nearRight && cornerTop) || (nearTop && cornerRight)) {
        return QRect(b.left() + halfW, b.top(), w - halfW, halfH);
    }
    if ((nearRight && cornerBottom) || (nearBottom && cornerRight)) {
        return QRect(b.left() + halfW, b.top() + halfH, w - halfW, h - halfH);
    }
    if (nearTop) {
        isMaximizeSnap = true;
        return b;
    }
    if (nearLeft) {
        return QRect(b.left(), b.top(), halfW, h);
    }
    if (nearRight) {
        return QRect(b.left() + halfW, b.top(), w - halfW, h);
    }
    return {};
}

void BrowserWindow::setupResizeHandles(QWidget *frame)
{
    for (auto &handle : resizeHandles_) {
        handle = new QWidget(frame);
        handle->setAttribute(Qt::WA_NoSystemBackground, true);
        handle->setStyleSheet(QStringLiteral("background: transparent;"));
        handle->setMouseTracking(true);
        handle->installEventFilter(this);
    }
    layoutResizeHandles();
}

void BrowserWindow::layoutResizeHandles()
{
    QWidget *frame = managedWindowFrame();
    if (!frame) {
        return;
    }

    const int w = frame->width();
    const int h = frame->height();

    if (!windowMaximized_ && !windowSnapped_ && w > 0 && h > 0) {
        QPainterPath path;
        path.addRoundedRect(QRectF(0, 0, w, h), kWindowCornerRadius, kWindowCornerRadius);
        frame->setMask(QRegion(path.toFillPolygon().toPolygon()));
    } else {
        frame->clearMask();
    }

    if (!resizeHandles_[0]) {
        return;
    }
    if (windowMaximized_) {
        for (auto *handle : resizeHandles_) {
            if (handle) handle->hide();
        }
        return;
    }

    const int t = kResizeBorder;
    // Left, Right, Top, Bottom transparent edge strips
    resizeHandles_[0]->setGeometry(0, 0, t, h);
    resizeHandles_[1]->setGeometry(qMax(0, w - t), 0, t, h);
    resizeHandles_[2]->setGeometry(0, 0, w, t);
    resizeHandles_[3]->setGeometry(0, qMax(0, h - t), w, t);
    for (auto *handle : resizeHandles_) {
        handle->show();
        handle->raise();
    }
}

bool BrowserWindow::isResizeHandle(QObject *watched) const
{
    for (auto *handle : resizeHandles_) {
        if (handle && watched == handle) {
            return true;
        }
    }
    return false;
}

void BrowserWindow::updateFrameMargins()
{
    QWidget *frame = managedWindowFrame();
    if (!frame || !frame->layout()) {
        return;
    }
    const QString bg = seb::ui::taskbar::backgroundColor().name();
    const QString objName = frame->objectName();
    frame->layout()->setContentsMargins(0, 0, 0, 0);
    if (!windowMaximized_ && !windowSnapped_) {
        frame->setStyleSheet(QStringLiteral(
            "#%1 { background: %2; border: none; border-radius: %3px; }")
                                 .arg(objName, bg)
                                 .arg(kWindowCornerRadius));
    } else {
        frame->setStyleSheet(QStringLiteral("#%1 { background: %2; border: none; }").arg(objName, bg));
    }
    if (maximizeButton_) {
        maximizeButton_->setIcon(windowMaximized_ ? buildRestoreIcon() : buildMaximizeIcon());
        maximizeButton_->setToolTip(windowMaximized_ ? QStringLiteral("Restore") : QStringLiteral("Maximize"));
    }
    layoutResizeHandles();
}

QFrame *BrowserWindow::createTitleBar(QWidget *parent)
{
    auto *bar = new QFrame(parent);
    bar->setObjectName(QStringLiteral("sebWindowTitleBar"));
    bar->setFixedHeight(kTitleBarHeight);
    bar->setAutoFillBackground(true);
    bar->setAttribute(Qt::WA_StyledBackground, true);

    const QString bg = seb::ui::taskbar::backgroundColor().name();
    bar->setStyleSheet(QStringLiteral(
        "#sebWindowTitleBar { background: %1; border: none; color: black; }")
                           .arg(bg));

    auto *layout = new QHBoxLayout(bar);
    layout->setContentsMargins(8, 0, 0, 0);
    layout->setSpacing(0);

    auto *iconLabel = new QLabel(bar);
    iconLabel->setPixmap(QPixmap(QStringLiteral(":/assets/icons/safe-exam-browser.png"))
                             .scaled(15, 15, Qt::KeepAspectRatio, Qt::SmoothTransformation));
    iconLabel->setAttribute(Qt::WA_TransparentForMouseEvents, true);
    iconLabel->setStyleSheet(QStringLiteral("background: transparent; border: none; padding-right: 6px;"));
    layout->addWidget(iconLabel);

    auto *nameLabel = new QLabel(QStringLiteral("Safe Exam Browser"), bar);
    nameLabel->setAttribute(Qt::WA_TransparentForMouseEvents, true);
    nameLabel->setStyleSheet(QStringLiteral(
        "background: transparent; border: none; color: black; font-size: 11px; font-weight: 600;"));
    layout->addWidget(nameLabel, 1);

    const QString btnStyle = taskbarButtonStyleSheet();

    auto *minimizeBtn = new QPushButton(QStringLiteral("−"), bar);
    minimizeBtn->setFixedSize(34, kTitleBarHeight);
    minimizeBtn->setToolTip(QStringLiteral("Minimize"));
    minimizeBtn->setCursor(Qt::PointingHandCursor);
    minimizeBtn->setFocusPolicy(Qt::NoFocus);
    minimizeBtn->setStyleSheet(btnStyle);
    connect(minimizeBtn, &QPushButton::clicked, this, &BrowserWindow::minimizeManagedWindow);
    layout->addWidget(minimizeBtn);

    maximizeButton_ = new QPushButton(bar);
    maximizeButton_->setFixedSize(34, kTitleBarHeight);
    maximizeButton_->setIcon(windowMaximized_ ? buildRestoreIcon() : buildMaximizeIcon());
    maximizeButton_->setIconSize(QSize(13, 13));
    maximizeButton_->setToolTip(windowMaximized_ ? QStringLiteral("Restore") : QStringLiteral("Maximize"));
    maximizeButton_->setCursor(Qt::PointingHandCursor);
    maximizeButton_->setFocusPolicy(Qt::NoFocus);
    maximizeButton_->setStyleSheet(btnStyle);
    connect(maximizeButton_, &QPushButton::clicked, this, &BrowserWindow::toggleManagedWindowMaximized);
    layout->addWidget(maximizeButton_);

    auto *closeBtn = new QPushButton(QStringLiteral("×"), bar);
    closeBtn->setFixedSize(34, kTitleBarHeight);
    closeBtn->setFocusPolicy(Qt::NoFocus);
    closeBtn->setStyleSheet(btnStyle);
    if (isMainWindow_) {
        closeBtn->setEnabled(false);
        closeBtn->setToolTip(QStringLiteral("Main window cannot be closed"));
    } else {
        closeBtn->setCursor(Qt::PointingHandCursor);
        closeBtn->setToolTip(QStringLiteral("Close Window"));
        connect(closeBtn, &QPushButton::clicked, this, &QWidget::close);
    }
    layout->addWidget(closeBtn);

    return bar;
}

QUrl BrowserWindow::currentUrl() const
{
    return view_ ? view_->url() : QUrl();
}

bool BrowserWindow::isMainWindow() const
{
    return isMainWindow_;
}

QWidget *BrowserWindow::workspaceWidget() const
{
    return workspace_;
}

void BrowserWindow::bringMainContentToFront()
{
    if (windowFrame_) {
        windowFrame_->show();
        windowFrame_->raise();
    }
}

bool BrowserWindow::shouldAllowNavigation(const QUrl &url)
{
    if (!url.isValid()) {
        return true;
    }

    if (session_.isQuitUrl(url)) {
        if (session_.settings().Browser.ResetOnQuitUrl) {
            const QUrl home = session_.homeUrl();
            if (home.isValid()) {
                view_->setUrl(home);
            }
            return false;
        }

        if (session_.settings().Browser.ConfirmQuitUrl && !session_.confirmQuitUrl(this)) {
            return false;
        }

        requestQuit(!session_.settings().Browser.ConfirmQuitUrl);
        return false;
    }

    if (!session_.isMainNavigationAllowed(url)) {
        return false;
    }

    if (url.isLocalFile()) {
        return false;
    }

    if (seb::browser::ConfigurationResourceDetector::isConfigurationResource(url)) {
        view_->download(seb::browser::ConfigurationResourceDetector::downloadUrl(url));
        return false;
    }

    return true;
}

QString BrowserWindow::taskbarIconPath() const
{
    return QStringLiteral(":/assets/icons/safe-exam-browser.png");
}

QString BrowserWindow::taskbarTitle() const
{
    const QString title = windowTitle().trimmed();
    return title.isEmpty() ? QStringLiteral("Safe Exam Browser") : title;
}

void BrowserWindow::changeEvent(QEvent *event)
{
    QWidget::changeEvent(event);
    notifyTaskbarStateChanged();
}

void BrowserWindow::closeEvent(QCloseEvent *event)
{
    if (!isMainWindow_) {
        event->accept();
        return;
    }

    event->ignore();
    requestQuit(true);
}

void BrowserWindow::keyPressEvent(QKeyEvent *event)
{
    const bool ctrl = event->modifiers().testFlag(Qt::ControlModifier);
    const bool shift = event->modifiers().testFlag(Qt::ShiftModifier);
    const bool alt = event->modifiers().testFlag(Qt::AltModifier);
    const bool meta = event->modifiers().testFlag(Qt::MetaModifier);

    if ((meta && !ctrl && !alt && event->key() == Qt::Key_A) ||
        (ctrl && shift && !alt && event->key() == Qt::Key_A)) {
        if (!event->isAutoRepeat()) {
            toggleActionCenter();
        }
        event->accept();
        return;
    }

    if (event->key() == Qt::Key_Escape && sidebar_ && sidebar_->isVisible()) {
        sidebar_->closeSidebar();
        event->accept();
        return;
    }

    if (event->key() == Qt::Key_F11) {
        event->accept();
        return;
    }

    if (!windowSettings_.AllowReloading &&
        (event->key() == Qt::Key_F5 || (ctrl && event->key() == Qt::Key_R))) {
        event->accept();
        return;
    }

    if (!windowSettings_.AllowBackwardNavigation &&
        ((alt && event->key() == Qt::Key_Left) || event->key() == Qt::Key_Backspace)) {
        event->accept();
        return;
    }

    if (!windowSettings_.AllowForwardNavigation && (alt && event->key() == Qt::Key_Right)) {
        event->accept();
        return;
    }

    if (!session_.settings().Browser.AllowPrint && ctrl && event->key() == Qt::Key_P) {
        event->accept();
        return;
    }

    if (!session_.settings().Browser.AllowFind && ctrl && event->key() == Qt::Key_F) {
        event->accept();
        return;
    }

    if (!session_.settings().Browser.AllowPageZoom &&
        ctrl &&
        (event->key() == Qt::Key_Plus || event->key() == Qt::Key_Equal || event->key() == Qt::Key_Minus ||
         event->key() == Qt::Key_0)) {
        event->accept();
        return;
    }

    if (!windowSettings_.AllowDeveloperConsole && event->key() == Qt::Key_F12) {
        event->accept();
        return;
    }

    if (ctrl && !alt && event->key() == Qt::Key_Q) {
        if (!event->isAutoRepeat()) {
            requestQuit(true);
        }
        event->accept();
        return;
    }

    QWidget::keyPressEvent(event);
}

void BrowserWindow::focusInEvent(QFocusEvent *event)
{
    QWidget::focusInEvent(event);
    if (QWidget *frame = managedWindowFrame()) {
        frame->raise();
    }
    notifyTaskbarStateChanged();
}

void BrowserWindow::focusOutEvent(QFocusEvent *event)
{
    QWidget::focusOutEvent(event);
}

void BrowserWindow::resizeEvent(QResizeEvent *event)
{
    QWidget::resizeEvent(event);
    if (isMainWindow_ && workspace_ && windowFrame_) {
        if (windowMaximized_) {
            windowFrame_->setGeometry(workspace_->rect());
        } else {
            QRect g = windowFrame_->geometry();
            const QRect bounds = workspace_->rect();
            g.setWidth(qMin(g.width(), bounds.width()));
            g.setHeight(qMin(g.height(), bounds.height()));
            g.moveLeft(qBound(0, g.left(), qMax(0, bounds.width() - g.width())));
            g.moveTop(qBound(0, g.top(), qMax(0, bounds.height() - g.height())));
            windowFrame_->setGeometry(g);
        }
    }
    if (sidebar_ && sidebar_->isVisible()) {
        sidebar_->setFixedSize(sidebar_->width(), height());
        sidebar_->move(mapToGlobal(QPoint(0, 0)));
    }
    layoutResizeHandles();
}

bool BrowserWindow::eventFilter(QObject *watched, QEvent *event)
{
    if (!event) {
        return QWidget::eventFilter(watched, event);
    }

    if (watched == workspace_ && event->type() == QEvent::Resize && windowFrame_) {
        if (windowMaximized_) {
            windowFrame_->setGeometry(workspace_->rect());
        }
        layoutResizeHandles();
    }

    QWidget *frame = managedWindowFrame();
    if (frame && watched == frame && event->type() == QEvent::Resize) {
        layoutResizeHandles();
    }

    if (frame && (watched == frame || watched == titleBar_ || isResizeHandle(watched))) {
        if (event->type() == QEvent::MouseMove && !draggingWindow_ && !resizingWindow_) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            const QPoint localInFrame = frame->mapFromGlobal(mouse->globalPosition().toPoint());
            const int edges = hitTestResizeEdges(localInFrame, frame->size());
            updateCursorForEdges(qobject_cast<QWidget *>(watched), edges);
        } else if (event->type() == QEvent::Leave && !resizingWindow_) {
            updateCursorForEdges(qobject_cast<QWidget *>(watched), ResizeNone);
        } else if (event->type() == QEvent::MouseButtonPress) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            if (mouse->button() == Qt::LeftButton) {
                frame->raise();
                const QPoint localInFrame = frame->mapFromGlobal(mouse->globalPosition().toPoint());
                const int edges = hitTestResizeEdges(localInFrame, frame->size());
                if (edges != ResizeNone) {
                    resizingWindow_ = true;
                    activeResizeEdges_ = edges;
                    resizeStartGlobalPos_ = mouse->globalPosition().toPoint();
                    resizeStartGeometry_ = frame->geometry();
                    windowSnapped_ = false;
                    return true;
                }
            }
        } else if (event->type() == QEvent::MouseMove && resizingWindow_) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            const QPoint delta = mouse->globalPosition().toPoint() - resizeStartGlobalPos_;
            const QRect bounds = workspaceBounds();
            QRect next = resizeStartGeometry_;

            if (activeResizeEdges_ & ResizeLeft) {
                const int newLeft = qBound(bounds.left(), resizeStartGeometry_.left() + delta.x(),
                                           resizeStartGeometry_.right() - kMinWindowWidth + 1);
                next.setLeft(newLeft);
            }
            if (activeResizeEdges_ & ResizeRight) {
                const int newRight = qBound(resizeStartGeometry_.left() + kMinWindowWidth - 1,
                                            resizeStartGeometry_.right() + delta.x(), bounds.right());
                next.setRight(newRight);
            }
            if (activeResizeEdges_ & ResizeTop) {
                const int newTop = qBound(bounds.top(), resizeStartGeometry_.top() + delta.y(),
                                          resizeStartGeometry_.bottom() - kMinWindowHeight + 1);
                next.setTop(newTop);
            }
            if (activeResizeEdges_ & ResizeBottom) {
                const int newBottom = qBound(resizeStartGeometry_.top() + kMinWindowHeight - 1,
                                             resizeStartGeometry_.bottom() + delta.y(), bounds.bottom());
                next.setBottom(newBottom);
            }

            frame->setGeometry(next);
            normalWindowGeometry_ = next;
            return true;
        } else if (event->type() == QEvent::MouseButtonRelease && resizingWindow_) {
            resizingWindow_ = false;
            activeResizeEdges_ = ResizeNone;
            updateCursorForEdges(frame, ResizeNone);
            updateCursorForEdges(titleBar_, ResizeNone);
            return true;
        }
    }

    if (watched == titleBar_ && frame && !resizingWindow_) {
        if (event->type() == QEvent::MouseButtonPress) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            if (mouse->button() == Qt::LeftButton) {
                frame->raise();
                dragPending_ = true;
                draggingWindow_ = false;
                dragStartGlobalPos_ = mouse->globalPosition().toPoint();
                dragStartLocalPos_ = mouse->position().toPoint();
                dragOffset_ = dragStartGlobalPos_ - frame->pos();
                activeSnapRect_ = QRect();
                activeSnapIsMaximize_ = false;
                return true;
            }
        } else if (event->type() == QEvent::MouseMove && (dragPending_ || draggingWindow_)) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            const QPoint globalPos = mouse->globalPosition().toPoint();
            if (dragPending_) {
                if ((globalPos - dragStartGlobalPos_).manhattanLength() < QApplication::startDragDistance()) {
                    return true;
                }
                dragPending_ = false;
                draggingWindow_ = true;
                if (windowMaximized_ || windowSnapped_) {
                    const QRect bounds = workspaceBounds();
                    const QRect target = normalWindowGeometry_.isValid() &&
                                                 normalWindowGeometry_.width() >= kMinWindowWidth &&
                                                 normalWindowGeometry_.width() < bounds.width()
                        ? normalWindowGeometry_
                        : QRect(bounds.left() + bounds.width() / 10,
                                bounds.top() + bounds.height() / 10,
                                (bounds.width() * 80) / 100,
                                (bounds.height() * 80) / 100);
                    const double ratio = frame->width() > 0
                        ? static_cast<double>(dragStartLocalPos_.x()) / frame->width()
                        : 0.5;
                    windowMaximized_ = false;
                    windowSnapped_ = false;
                    updateFrameMargins();
                    frame->resize(target.size());
                    const QPoint localInWorkspace = frame->parentWidget()
                        ? frame->parentWidget()->mapFromGlobal(globalPos)
                        : globalPos;
                    const int newX = qBound(0,
                                            localInWorkspace.x() - static_cast<int>(target.width() * ratio),
                                            qMax(0, bounds.width() - target.width()));
                    const int newY = qBound(0,
                                            localInWorkspace.y() - dragStartLocalPos_.y(),
                                            qMax(0, bounds.height() - target.height()));
                    frame->move(newX, newY);
                    normalWindowGeometry_ = frame->geometry();
                    dragOffset_ = globalPos - frame->pos();
                }
            }

            QPoint targetPos = globalPos - dragOffset_;
            const QRect bounds = workspaceBounds();
            constexpr int kMinVisibleStrip = 80;
            const int minX = bounds.left() - frame->width() + kMinVisibleStrip;
            const int maxX = bounds.right() - kMinVisibleStrip;
            const int minY = bounds.top();
            const int maxY = bounds.bottom() - kTitleBarHeight;
            targetPos.setX(qBound(minX, targetPos.x(), maxX));
            targetPos.setY(qBound(minY, targetPos.y(), maxY));
            frame->move(targetPos);
            normalWindowGeometry_ = frame->geometry();

            const QPoint workspaceCursor = frame->parentWidget()
                ? frame->parentWidget()->mapFromGlobal(globalPos)
                : globalPos;
            bool isMaxSnap = false;
            const QRect snapRect = computeSnapRect(workspaceCursor, isMaxSnap);
            activeSnapRect_ = snapRect;
            activeSnapIsMaximize_ = isMaxSnap;
            if (snapRect.isValid() && !snapRect.isEmpty()) {
                showSnapPreview(snapRect, frame);
            } else {
                hideSnapPreview();
            }
            return true;
        } else if (event->type() == QEvent::MouseButtonRelease && (dragPending_ || draggingWindow_)) {
            const bool wasDragging = draggingWindow_;
            dragPending_ = false;
            draggingWindow_ = false;
            hideSnapPreview();
            if (wasDragging && activeSnapRect_.isValid() && !activeSnapRect_.isEmpty()) {
                if (activeSnapIsMaximize_) {
                    windowMaximized_ = true;
                    windowSnapped_ = false;
                } else {
                    windowMaximized_ = false;
                    windowSnapped_ = true;
                }
                updateFrameMargins();
                frame->setGeometry(activeSnapRect_);
                frame->raise();
            }
            activeSnapRect_ = QRect();
            activeSnapIsMaximize_ = false;
            return true;
        } else if (event->type() == QEvent::MouseButtonDblClick) {
            auto *mouse = static_cast<QMouseEvent *>(event);
            if (mouse->button() == Qt::LeftButton) {
                dragPending_ = false;
                draggingWindow_ = false;
                activeSnapRect_ = QRect();
                activeSnapIsMaximize_ = false;
                hideSnapPreview();
                toggleManagedWindowMaximized();
                return true;
            }
        }
    }

    if (event->type() == QEvent::MouseButtonPress) {
        if (!isMainWindow_) {
            raise();
        } else if (view_ && (watched == view_->widget() || watched == windowFrame_)) {
            bringMainContentToFront();
        }
    }

    if (event->type() == QEvent::KeyPress) {
        auto *key = static_cast<QKeyEvent *>(event);
        const bool meta = key->modifiers().testFlag(Qt::MetaModifier);
        const bool ctrl = key->modifiers().testFlag(Qt::ControlModifier);
        const bool shift = key->modifiers().testFlag(Qt::ShiftModifier);
        const bool alt = key->modifiers().testFlag(Qt::AltModifier);
        if ((meta && !ctrl && !alt && key->key() == Qt::Key_A) ||
            (ctrl && shift && !alt && key->key() == Qt::Key_A)) {
            if (!key->isAutoRepeat()) {
                toggleActionCenter();
            }
            event->accept();
            return true;
        }
        if (key->key() == Qt::Key_Escape) {
            if (findPopup_ && findPopup_->isVisible()) {
                findPopup_->hide();
                if (view_) view_->findText(QString());
                event->accept();
                return true;
            }
            if (zoomPopup_ && zoomPopup_->isVisible()) {
                zoomPopup_->hide();
                event->accept();
                return true;
            }
            if (sidebar_ && sidebar_->isVisible()) {
                sidebar_->closeSidebar();
                event->accept();
                return true;
            }
        }
    }
    return QWidget::eventFilter(watched, event);
}

void BrowserWindow::applyWindowFlags()
{
    if (isMainWindow_ || !parentWidget()) {
        setWindowFlags(Qt::Window | Qt::FramelessWindowHint | Qt::CustomizeWindowHint);
    }
}

void BrowserWindow::applyWindowGeometry()
{
    const QScreen *screen = QGuiApplication::primaryScreen();
    const QRect full = screen ? screen->geometry() : QRect(0, 0, 1280, 800);

    if (isMainWindow_) {
        setGeometry(full);
        showFullScreen();
        if (workspace_ && windowFrame_) {
            const QRect bounds = workspace_->rect();
            windowMaximized_ = true;
            windowFrame_->setGeometry(bounds);
            updateFrameMargins();
            normalWindowGeometry_ = QRect(
                bounds.width() / 10,
                bounds.height() / 10,
                qMax(480, (bounds.width() * 80) / 100),
                qMax(360, (bounds.height() * 80) / 100));
        }
        return;
    }

    const QRect bounds = parentWidget() ? parentWidget()->rect() : full;

    int width = bounds.width();
    int height = bounds.height();

    if (windowSettings_.AbsoluteWidth.has_value()) {
        width = *windowSettings_.AbsoluteWidth;
    } else if (windowSettings_.RelativeWidth.has_value()) {
        width = (bounds.width() * *windowSettings_.RelativeWidth) / 100;
    } else {
        width = (bounds.width() * 70) / 100;
    }

    if (windowSettings_.AbsoluteHeight.has_value()) {
        height = *windowSettings_.AbsoluteHeight;
    } else if (windowSettings_.RelativeHeight.has_value()) {
        height = (bounds.height() * *windowSettings_.RelativeHeight) / 100;
    } else {
        height = (bounds.height() * 80) / 100;
    }

    width = qBound(320, width, bounds.width());
    height = qBound(240, height, bounds.height());
    resize(width, height);

    int x = bounds.x();
    switch (windowSettings_.Position) {
    case seb::settings::browser::WindowPosition::Left:
        x = bounds.left();
        break;
    case seb::settings::browser::WindowPosition::Center:
        x = bounds.left() + (bounds.width() - width) / 2;
        break;
    case seb::settings::browser::WindowPosition::Right:
        x = bounds.right() - width + 1;
        break;
    }

    const int y = bounds.top() + (bounds.height() - height) / 2;
    move(x, y);
    windowMaximized_ = false;
    normalWindowGeometry_ = geometry();
    updateFrameMargins();
}

void BrowserWindow::configureToolbar(QWidget *toolbarHost)
{
    const bool showUrlAlways = windowSettings_.UrlPolicy == seb::settings::browser::UrlPolicy::Always;
    const bool showToolbar =
        windowSettings_.ShowToolbar ||
        windowSettings_.AllowAddressBar ||
        windowSettings_.ShowHomeButton ||
        showUrlAlways;

    if (!showToolbar) {
        return;
    }

    toolbar_ = new QToolBar(QStringLiteral("Browser"), toolbarHost);
    toolbar_->setMovable(false);
    toolbar_->setFloatable(false);
    toolbar_->setContextMenuPolicy(Qt::PreventContextMenu);
    toolbar_->setIconSize(QSize(seb::ui::taskbar::kTaskbarIconSize, seb::ui::taskbar::kTaskbarIconSize));
    toolbar_->setFixedHeight(seb::ui::taskbar::kTaskbarHeight);

    const QString bg = seb::ui::taskbar::backgroundColor().name();
    const QString hoverBg = seb::ui::taskbar::hoverBackgroundColor().name();
    const QString hoverBorder = seb::ui::taskbar::hoverBorderColor().name();
    const QString pressedBorder = seb::ui::taskbar::pressedBorderColor().name();

    QPalette toolbarPalette = toolbar_->palette();
    toolbarPalette.setColor(QPalette::Window, seb::ui::taskbar::backgroundColor());
    toolbarPalette.setColor(QPalette::Base, seb::ui::taskbar::backgroundColor());
    toolbarPalette.setColor(QPalette::Button, seb::ui::taskbar::backgroundColor());
    toolbarPalette.setColor(QPalette::WindowText, Qt::black);
    toolbarPalette.setColor(QPalette::Text, Qt::black);
    toolbarPalette.setColor(QPalette::ButtonText, Qt::black);
    toolbar_->setPalette(toolbarPalette);

    toolbar_->setStyleSheet(QStringLiteral(
        "QToolBar { background: %1; color: black; border: none; spacing: 0px; padding: 0px; }"
        "QToolButton { background: %1; color: black; border: 1px solid %1; border-radius: 0px; padding: 0px; min-width: 40px; min-height: 38px; }"
        "QToolButton:hover { background: %2; border: 1px solid %3; color: black; }"
        "QToolButton:pressed { background: %2; border: 2px solid %4; color: black; }"
        "QToolButton:disabled { background: %1; color: rgba(0, 0, 0, 64); border: 1px solid %1; }"
        "QLineEdit { background: %1; color: black; border: 1px solid gray; border-radius: 0px; padding: 4px 8px; margin: 4px 6px; selection-background-color: %2; selection-color: black; }"
        "QLineEdit:focus { border: 1px solid %3; }"
        "QLineEdit:read-only { background: lightgray; color: black; border: 1px solid gray; }"
        "QPushButton { background: %1; color: black; border: 1px solid %1; border-radius: 0px; padding: 0px 8px; font-size: 12px; }"
        "QPushButton:hover { background: %2; border: 1px solid %3; color: black; }"
        "QPushButton:pressed { background: %2; border: 2px solid %4; color: black; }"
        "QPushButton:disabled { background: %1; color: rgba(0, 0, 0, 64); border: 1px solid %1; }")
                                .arg(bg, hoverBg, hoverBorder, pressedBorder));

    if (windowSettings_.AllowBackwardNavigation) {
        backAction_ = toolbar_->addAction(QIcon(QStringLiteral(":/assets/taskbar/back.svg")), QStringLiteral("Back"));
        backAction_->setToolTip(QStringLiteral("Back (Alt+Left)"));
        backAction_->setEnabled(false);
        connect(backAction_, &QAction::triggered, this, [this] {
            if (windowSettings_.AllowBackwardNavigation && view_) {
                view_->back();
            }
        });
    }

    if (windowSettings_.AllowForwardNavigation) {
        forwardAction_ = toolbar_->addAction(QIcon(QStringLiteral(":/assets/taskbar/forward.svg")), QStringLiteral("Forward"));
        forwardAction_->setToolTip(QStringLiteral("Forward (Alt+Right)"));
        forwardAction_->setEnabled(false);
        connect(forwardAction_, &QAction::triggered, this, [this] {
            if (windowSettings_.AllowForwardNavigation && view_) {
                view_->forward();
            }
        });
    }

    if (windowSettings_.ShowReloadButton && windowSettings_.AllowReloading) {
        reloadAction_ = toolbar_->addAction(QIcon(QStringLiteral(":/assets/taskbar/reload.svg")), QStringLiteral("Reload"));
        reloadAction_->setToolTip(QStringLiteral("Reload Page (F5 / Ctrl+R)"));
        reloadAction_->setEnabled(true);
        connect(reloadAction_, &QAction::triggered, this, &BrowserWindow::reloadPage);
    }

    if (windowSettings_.ShowHomeButton && session_.homeUrl().isValid()) {
        homeAction_ = toolbar_->addAction(QIcon(QStringLiteral(":/assets/taskbar/home.svg")), QStringLiteral("Home"));
        homeAction_->setToolTip(QStringLiteral("Home / Restart Exam (Alt+Home)"));
        connect(homeAction_, &QAction::triggered, this, &BrowserWindow::navigateHome);
    }

    if (windowSettings_.AllowAddressBar || showUrlAlways) {
        addressBar_ = new QLineEdit(toolbar_);
        addressBar_->setReadOnly(!windowSettings_.AllowAddressBar);
        addressBar_->setClearButtonEnabled(windowSettings_.AllowAddressBar);
        addressBar_->setPlaceholderText(QStringLiteral("Enter exam URL"));
        addressBar_->setMinimumHeight(28);
        addressBar_->installEventFilter(this);
        toolbar_->addWidget(addressBar_);
        if (windowSettings_.AllowAddressBar) {
            connect(addressBar_, &QLineEdit::returnPressed, this, [this] {
                QUrl entered = QUrl::fromUserInput(addressBar_->text().trimmed());
                if (entered.scheme().compare(QStringLiteral("seb"), Qt::CaseInsensitive) == 0) {
                    entered.setScheme(QStringLiteral("http"));
                } else if (entered.scheme().compare(QStringLiteral("sebs"), Qt::CaseInsensitive) == 0) {
                    entered.setScheme(QStringLiteral("https"));
                }
                if (view_ && entered.isValid()) {
                    view_->setUrl(entered);
                }
            });
        }
    } else {
        titleLabel_ = new QLabel(toolbar_);
        titleLabel_->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Preferred);
        titleLabel_->setStyleSheet(QStringLiteral("color: black; font-size: 13px; font-weight: 600; padding: 0 8px; background: transparent;"));
        titleLabel_->setText(windowTitle().isEmpty() ? QStringLiteral("Safe Exam Browser") : windowTitle());
        toolbar_->addWidget(titleLabel_);
    }

    if (session_.settings().Browser.AllowPageZoom) {
        zoomMenuButton_ = new QToolButton(toolbar_);
        zoomMenuButton_->setIcon(buildZoomIcon());
        zoomMenuButton_->setIconSize(QSize(20, 20));
        zoomMenuButton_->setToolTip(QStringLiteral("Zoom"));
        zoomMenuButton_->setCursor(Qt::PointingHandCursor);
        zoomMenuButton_->setFocusPolicy(Qt::NoFocus);
        connect(zoomMenuButton_, &QToolButton::clicked, this, &BrowserWindow::toggleZoomPopup);
        toolbar_->addWidget(zoomMenuButton_);
    }

    if (session_.settings().Browser.AllowFind) {
        findMenuButton_ = new QToolButton(toolbar_);
        findMenuButton_->setIcon(buildSearchIcon());
        findMenuButton_->setIconSize(QSize(20, 20));
        findMenuButton_->setToolTip(QStringLiteral("Find in Page (Ctrl+F)"));
        findMenuButton_->setCursor(Qt::PointingHandCursor);
        findMenuButton_->setFocusPolicy(Qt::NoFocus);
        connect(findMenuButton_, &QToolButton::clicked, this, &BrowserWindow::toggleFindPopup);
        toolbar_->addWidget(findMenuButton_);
    }
}

void BrowserWindow::configureSidebar()
{
    sidebar_ = new seb::ui::ActionCenterSidebar(session_, this);
    connect(sidebar_, &seb::ui::ActionCenterSidebar::quitRequested, this, [this] { requestQuit(true); });
}

void BrowserWindow::toggleActionCenter()
{
    if (!isMainWindow_) {
        if (BrowserWindow *main = session_.mainWindow()) {
            main->toggleActionCenter();
        }
        return;
    }
    if (!sidebar_) {
        return;
    }
    if (sidebarToggleDebounce_.isValid() && sidebarToggleDebounce_.elapsed() < 200) {
        return;
    }
    sidebarToggleDebounce_.restart();
    sidebar_->toggleSidebar();
}

void BrowserWindow::toggleZoomPopup()
{
    if (!session_.settings().Browser.AllowPageZoom) {
        return;
    }

    if (!zoomPopup_) {
        zoomPopup_ = new QFrame(this, Qt::Popup | Qt::FramelessWindowHint);
        zoomPopup_->setStyleSheet(QStringLiteral(
            "QFrame { background: %1; border: 1px solid gray; color: black; }")
                                      .arg(seb::ui::taskbar::backgroundColor().name()));

        auto *layout = new QHBoxLayout(zoomPopup_);
        layout->setContentsMargins(4, 4, 4, 4);
        layout->setSpacing(2);

        const QString btnStyle = taskbarButtonStyleSheet();

        auto *minusBtn = new QPushButton(QStringLiteral("−"), zoomPopup_);
        minusBtn->setFixedSize(34, 32);
        minusBtn->setCursor(Qt::PointingHandCursor);
        minusBtn->setFocusPolicy(Qt::NoFocus);
        minusBtn->setStyleSheet(btnStyle);
        connect(minusBtn, &QPushButton::clicked, this, [this] { changeZoom(-1); });

        zoomLevelButton_ = new QPushButton(QStringLiteral("%1%").arg(zoomPercentage_), zoomPopup_);
        zoomLevelButton_->setFixedSize(62, 32);
        zoomLevelButton_->setCursor(Qt::PointingHandCursor);
        zoomLevelButton_->setFocusPolicy(Qt::NoFocus);
        zoomLevelButton_->setToolTip(QStringLiteral("Reset Zoom (100%)"));
        zoomLevelButton_->setStyleSheet(btnStyle);
        connect(zoomLevelButton_, &QPushButton::clicked, this, [this] { changeZoom(0); });

        auto *plusBtn = new QPushButton(QStringLiteral("+"), zoomPopup_);
        plusBtn->setFixedSize(34, 32);
        plusBtn->setCursor(Qt::PointingHandCursor);
        plusBtn->setFocusPolicy(Qt::NoFocus);
        plusBtn->setStyleSheet(btnStyle);
        connect(plusBtn, &QPushButton::clicked, this, [this] { changeZoom(1); });

        layout->addWidget(minusBtn);
        layout->addWidget(zoomLevelButton_);
        layout->addWidget(plusBtn);
    }

    if (zoomPopup_->isVisible()) {
        zoomPopup_->hide();
        return;
    }

    if (zoomLevelButton_) {
        zoomLevelButton_->setText(QStringLiteral("%1%").arg(zoomPercentage_));
    }
    zoomPopup_->adjustSize();
    if (zoomMenuButton_) {
        const QPoint anchor = zoomMenuButton_->mapToGlobal(
            QPoint(zoomMenuButton_->width() - zoomPopup_->width(), zoomMenuButton_->height() + 2));
        zoomPopup_->move(anchor);
    }
    zoomPopup_->show();
    zoomPopup_->raise();
}

void BrowserWindow::toggleFindPopup()
{
    if (!session_.settings().Browser.AllowFind) {
        return;
    }

    if (!findPopup_) {
        findPopup_ = new QFrame(this, Qt::Popup | Qt::FramelessWindowHint);
        findPopup_->setStyleSheet(QStringLiteral(
            "QFrame { background: %1; border: 1px solid gray; color: black; }")
                                      .arg(seb::ui::taskbar::backgroundColor().name()));

        auto *layout = new QHBoxLayout(findPopup_);
        layout->setContentsMargins(6, 6, 6, 6);
        layout->setSpacing(4);

        findLineEdit_ = new QLineEdit(findPopup_);
        findLineEdit_->setPlaceholderText(QStringLiteral("Find"));
        findLineEdit_->setFixedSize(210, 30);
        findLineEdit_->setClearButtonEnabled(true);
        findLineEdit_->setStyleSheet(QStringLiteral(
            "QLineEdit { background: %1; color: black; border: 1px solid gray; padding: 3px 6px; "
            "selection-background-color: %2; selection-color: black; }"
            "QLineEdit:focus { border: 1px solid %3; }")
                                         .arg(seb::ui::taskbar::backgroundColor().name(),
                                              seb::ui::taskbar::hoverBackgroundColor().name(),
                                              seb::ui::taskbar::hoverBorderColor().name()));
        findLineEdit_->installEventFilter(this);

        connect(findLineEdit_, &QLineEdit::returnPressed, this, [this] {
            if (view_) {
                view_->findText(findLineEdit_->text());
            }
        });
        connect(findLineEdit_, &QLineEdit::textChanged, this, [this](const QString &text) {
            if (view_) {
                view_->findText(text);
            }
        });

        auto *closeBtn = new QPushButton(QStringLiteral("×"), findPopup_);
        closeBtn->setFixedSize(30, 30);
        closeBtn->setCursor(Qt::PointingHandCursor);
        closeBtn->setFocusPolicy(Qt::NoFocus);
        closeBtn->setStyleSheet(taskbarButtonStyleSheet());
        connect(closeBtn, &QPushButton::clicked, this, [this] {
            findPopup_->hide();
            if (view_) {
                view_->findText(QString());
            }
        });

        layout->addWidget(findLineEdit_);
        layout->addWidget(closeBtn);
    }

    if (findPopup_->isVisible()) {
        findPopup_->hide();
        return;
    }

    findPopup_->adjustSize();
    if (findMenuButton_) {
        const QPoint anchor = findMenuButton_->mapToGlobal(
            QPoint(findMenuButton_->width() - findPopup_->width(), findMenuButton_->height() + 2));
        findPopup_->move(anchor);
    } else if (QWidget *frame = managedWindowFrame()) {
        const QPoint anchor = frame->mapToGlobal(QPoint(frame->width() - findPopup_->width() - 12, kTitleBarHeight + 8));
        findPopup_->move(anchor);
    }
    findPopup_->show();
    findPopup_->raise();
    findLineEdit_->setFocus();
    findLineEdit_->selectAll();
}

void BrowserWindow::minimizeManagedWindow()
{
    if (QWidget *frame = managedWindowFrame()) {
        frame->hide();
    }
}

void BrowserWindow::toggleManagedWindowMaximized()
{
    QWidget *frame = managedWindowFrame();
    if (!frame) {
        return;
    }
    const QRect bounds = workspaceBounds();
    if (windowMaximized_) {
        QRect target = normalWindowGeometry_;
        if (!target.isValid() || target.width() >= bounds.width() || target.height() >= bounds.height()) {
            target = QRect(
                bounds.left() + bounds.width() / 10,
                bounds.top() + bounds.height() / 10,
                qMax(480, (bounds.width() * 80) / 100),
                qMax(360, (bounds.height() * 80) / 100));
        }
        windowMaximized_ = false;
        windowSnapped_ = false;
        updateFrameMargins();
        frame->setGeometry(target);
    } else {
        if (!windowSnapped_) {
            normalWindowGeometry_ = frame->geometry();
        }
        windowMaximized_ = true;
        windowSnapped_ = false;
        updateFrameMargins();
        frame->setGeometry(bounds);
    }
    frame->raise();
}

void BrowserWindow::configureShortcuts()
{
    auto *reloadAction = new QAction(this);
    reloadAction->setShortcuts(QKeySequence::Refresh);
    addAction(reloadAction);
    connect(reloadAction, &QAction::triggered, this, &BrowserWindow::reloadPage);

    auto *quitAction = new QAction(this);
    quitAction->setShortcuts({QKeySequence(Qt::CTRL | Qt::Key_Q),
                              QKeySequence(Qt::CTRL | Qt::SHIFT | Qt::Key_Q)});
    addAction(quitAction);
    connect(quitAction, &QAction::triggered, this, [this] { requestQuit(true); });

    auto *backAction = new QAction(this);
    backAction->setShortcut(QKeySequence::Back);
    addAction(backAction);
    connect(backAction, &QAction::triggered, this, [this] {
        if (windowSettings_.AllowBackwardNavigation && view_) {
            view_->back();
        }
    });

    auto *forwardAction = new QAction(this);
    forwardAction->setShortcut(QKeySequence::Forward);
    addAction(forwardAction);
    connect(forwardAction, &QAction::triggered, this, [this] {
        if (windowSettings_.AllowForwardNavigation && view_) {
            view_->forward();
        }
    });

    auto *findAction = new QAction(this);
    findAction->setShortcut(QKeySequence::Find);
    addAction(findAction);
    connect(findAction, &QAction::triggered, this, &BrowserWindow::findInPage);

    auto *devToolsAction = new QAction(this);
    devToolsAction->setShortcut(QKeySequence(Qt::Key_F12));
    addAction(devToolsAction);
    connect(devToolsAction, &QAction::triggered, this, [this] {
        if (windowSettings_.AllowDeveloperConsole && view_) {
            view_->openDevTools();
        }
    });

    using Command = seb::browser::contracts::IWebView::Command;
    const QList<QPair<QKeySequence, Command>> commands{
        {QKeySequence(Qt::CTRL | Qt::Key_Plus), Command::ZoomIn},
        {QKeySequence(Qt::CTRL | Qt::Key_Equal), Command::ZoomIn},
        {QKeySequence(Qt::CTRL | Qt::Key_Minus), Command::ZoomOut},
        {QKeySequence(Qt::CTRL | Qt::Key_0), Command::ZoomReset},
        {QKeySequence::Print, Command::Print},
        {QKeySequence(Qt::ALT | Qt::Key_Home), Command::Home}};
    for (const auto &entry : commands) {
        auto *action = new QAction(this);
        action->setShortcut(entry.first);
        addAction(action);
        connect(action, &QAction::triggered, this, [this, command = entry.second] { handleBrowserCommand(command); });
    }

    if (windowSettings_.AllowAddressBar && addressBar_) {
        auto *focusAddressBar = new QAction(this);
        focusAddressBar->setShortcut(QKeySequence(Qt::CTRL | Qt::Key_L));
        addAction(focusAddressBar);
        connect(focusAddressBar, &QAction::triggered, this, [this] {
            addressBar_->setFocus();
            addressBar_->selectAll();
        });
    }
}

void BrowserWindow::findInPage()
{
    toggleFindPopup();
}

void BrowserWindow::navigateHome()
{
    const QUrl home = session_.homeUrl();
    if (!home.isValid()) {
        return;
    }

    if (!session_.promptForHomeNavigation(this)) {
        return;
    }

    if (view_) {
        view_->setUrl(home);
        updateAddressBar(home);
    }
}

void BrowserWindow::handleNewWindowRequest(const QUrl &target, seb::browser::contracts::IWebView *&targetView)
{
    bool openInSameWindow = false;
    if (!session_.isMainNavigationAllowed(target) ||
        !session_.isPopupAllowed(view_->url(), target, &openInSameWindow)) return;
    if (!shouldAllowNavigation(target)) return;
    if (openInSameWindow) {
        targetView = view_.get();
        return;
    }
    auto *window = session_.createWindow(QUrl(), false);
    targetView = window->view_.get();
    window->show();
    window->raise();
}

void BrowserWindow::changeZoom(int steps)
{
    if (!session_.settings().Browser.AllowPageZoom) return;
    zoomPercentage_ = steps == 0 ? 100 : qBound(25, zoomPercentage_ + steps * 5, 300);
    if (view_) {
        view_->setZoomFactor(zoomPercentage_ / 100.0);
    }
    if (zoomLevelButton_) {
        zoomLevelButton_->setText(QStringLiteral("%1%").arg(zoomPercentage_));
    }
}

void BrowserWindow::requestQuit(bool confirmWithoutPassword)
{
    if (quitPromptActive_) {
        return;
    }
    quitPromptActive_ = true;
    const bool approved = session_.requestApplicationQuit(
        this,
        QStringLiteral("Enter the quit password to close this exam session."),
        confirmWithoutPassword);
    quitPromptActive_ = false;
    if (approved) {
        session_.requestShutdown();
    }
}

void BrowserWindow::handleBrowserCommand(seb::browser::contracts::IWebView::Command command)
{
    if (!isEnabled()) return;
    using Command = seb::browser::contracts::IWebView::Command;
    switch (command) {
    case Command::Back: if (windowSettings_.AllowBackwardNavigation && view_) view_->back(); break;
    case Command::Forward: if (windowSettings_.AllowForwardNavigation && view_) view_->forward(); break;
    case Command::Reload: reloadPage(); break;
    case Command::Find: findInPage(); break;
    case Command::Home: navigateHome(); break;
    case Command::FocusAddressBar:
        if (windowSettings_.AllowAddressBar && addressBar_) { addressBar_->setFocus(); addressBar_->selectAll(); }
        break;
    case Command::ZoomIn: changeZoom(1); break;
    case Command::ZoomOut: changeZoom(-1); break;
    case Command::ZoomReset: changeZoom(0); break;
    case Command::Print: if (session_.settings().Browser.AllowPrint && view_) view_->print(); break;
    case Command::DeveloperTools: if (windowSettings_.AllowDeveloperConsole && view_) view_->openDevTools(); break;
    case Command::Quit:
        requestQuit(true);
        break;
    case Command::ToggleActionCenter:
        toggleActionCenter();
        break;
    }
}

void BrowserWindow::reloadPage()
{
    if (!windowSettings_.AllowReloading || reloadConfirmActive_) {
        return;
    }

    if (windowSettings_.ShowReloadWarning) {
        reloadConfirmActive_ = true;
        const bool confirmed = session_.confirmReload(this);
        reloadConfirmActive_ = false;
        if (!confirmed) {
            return;
        }
    }

    if (view_) {
        view_->reload();
    }
}

void BrowserWindow::updateAddressBar(const QUrl &url)
{
    if (addressBar_) {
        addressBar_->setText(url.toString());
    }
    if (taskbar_) {
        taskbar_->setCurrentUrl(url);
    }
    updateToolbarState();
    notifyTaskbarStateChanged();
}

void BrowserWindow::updateToolbarState()
{
    if (backAction_) {
        backAction_->setEnabled(windowSettings_.AllowBackwardNavigation && view_ && view_->canGoBack());
    }
    if (forwardAction_) {
        forwardAction_->setEnabled(windowSettings_.AllowForwardNavigation && view_ && view_->canGoForward());
    }
    if (reloadAction_) {
        reloadAction_->setEnabled(windowSettings_.AllowReloading);
    }
    if (titleLabel_) {
        const QString title = windowTitle().trimmed().isEmpty()
            ? QStringLiteral("Safe Exam Browser")
            : windowTitle().trimmed();
        if (windowSettings_.UrlPolicy == seb::settings::browser::UrlPolicy::BeforeTitle &&
            view_ && view_->url().isValid() && !view_->url().isEmpty()) {
            titleLabel_->setText(QStringLiteral("%1  —  %2").arg(view_->url().toString(), title));
        } else {
            titleLabel_->setText(title);
        }
    }
}

void BrowserWindow::notifyTaskbarStateChanged()
{
    session_.notifyBrowserWindowStateChanged(this);
}
