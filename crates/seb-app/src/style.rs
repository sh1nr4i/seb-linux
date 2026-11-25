use gtk::prelude::*;

/// Strict SEB light-theme palette matching AGENTS.md & taskbar_style.h:
/// #f0f0f0, #696969, black, lightgray, gray, #b0b0b0, #d8d8d8, #add8e6, #1e90ff, #87ceeb
pub const SEB_LIGHT_CSS: &str = r#"
* {
    -gtk-icon-style: regular;
    transition-property: none;
    transition-duration: 0ms;
}

button, entry, menuitem, list row, scrollbar slider {
    transition-property: none;
    transition-duration: 0ms;
}

* selection {
    background-color: #add8e6;
    color: black;
}

window.seb-shell,
window.background.csd.seb-shell,
.seb-dialog,
window.seb-dialog,
window.background.csd.seb-dialog,
dialog.seb-dialog,
dialog.background.csd.seb-dialog,
messagedialog.seb-dialog,
filechooserdialog.seb-dialog,
filechooserdialog.background.csd.seb-dialog,
window.seb-splash,
window.background.csd.seb-splash,
.background.csd {
    background-color: #f0f0f0;
    background-image: none;
    color: black;
    border: none;
    border-radius: 0px;
    box-shadow: none;
    outline: none;
    -gtk-outline-radius: 0px;
}

decoration,
decoration:backdrop,
window.seb-shell decoration,
window.seb-dialog decoration,
window.seb-splash decoration,
dialog.seb-dialog decoration,
filechooserdialog.seb-dialog decoration {
    box-shadow: none;
    margin: 0px;
    padding: 0px;
    border: none;
    border-radius: 0px;
}

.seb-modal-scrim {
    background-color: rgba(0, 0, 0, 0.28);
}

window.seb-dialog label,
dialog.seb-dialog label,
messagedialog.seb-dialog label,
filechooserdialog.seb-dialog label,
.seb-dialog label,
window.seb-dialog checkbutton,
dialog.seb-dialog checkbutton,
.seb-dialog checkbutton,
window.seb-dialog radiobutton,
dialog.seb-dialog radiobutton,
.seb-dialog radiobutton {
    color: black;
}

window.seb-dialog button,
dialog.seb-dialog button,
messagedialog.seb-dialog button,
filechooserdialog.seb-dialog button,
.seb-dialog button,
window.seb-dialog button.default,
dialog.seb-dialog button.default,
.seb-dialog button.default,
dialog.seb-dialog button.suggested-action,
dialog.seb-dialog button.destructive-action {
    background: #f0f0f0;
    background-image: none;
    color: black;
    border: 1px solid gray;
    border-radius: 0px;
    padding: 5px 14px;
    min-width: 72px;
    box-shadow: none;
    text-shadow: none;
    outline: none;
}

window.seb-dialog button:hover,
dialog.seb-dialog button:hover,
messagedialog.seb-dialog button:hover,
filechooserdialog.seb-dialog button:hover,
.seb-dialog button:hover {
    background: #add8e6;
    background-image: none;
    border: 1px solid #1e90ff;
    color: black;
}

window.seb-dialog button:focus,
window.seb-dialog button.seb-btn-focused,
dialog.seb-dialog button:focus,
dialog.seb-dialog button.seb-btn-focused,
messagedialog.seb-dialog button:focus,
filechooserdialog.seb-dialog button:focus,
.seb-dialog button:focus,
.seb-dialog button.seb-btn-focused {
    background: #add8e6;
    background-image: none;
    border: 2px solid #1e90ff;
    color: black;
    outline: none;
}

window.seb-dialog button:active,
dialog.seb-dialog button:active,
messagedialog.seb-dialog button:active,
filechooserdialog.seb-dialog button:active,
.seb-dialog button:active {
    background: #add8e6;
    background-image: none;
    border: 2px solid #87ceeb;
    color: black;
}

window.seb-dialog button:disabled,
dialog.seb-dialog button:disabled,
.seb-dialog button:disabled {
    background: #f0f0f0;
    color: #696969;
    border: 1px solid #b0b0b0;
}

dialog.seb-dialog buttonbox {
    padding: 0px 14px 12px 14px;
}

window.seb-splash,
window.background.csd.seb-splash {
    border: none;
    border-radius: 0px;
    box-shadow: none;
}

.seb-splash-accent {
    background-color: #1e90ff;
    min-height: 3px;
}

progressbar trough {
    background-color: #d8d8d8;
    border: 1px solid #b0b0b0;
    border-radius: 0px;
}

progressbar progress {
    background-color: #1e90ff;
    border-radius: 0px;
}

.seb-workspace {
    background-color: #d8d8d8;
}

.seb-pane-host {
    background-color: transparent;
    background-image: none;
    border: none;
    box-shadow: none;
}

.seb-window-frame {
    background-color: #f0f0f0;
    border: none;
    border-radius: 0px;
}

.seb-window-frame.windowed {
    border: 1px solid gray;
    border-radius: 8px;
}

.seb-window-frame.snapped {
    border: 1px solid gray;
    border-radius: 0px;
}

.seb-titlebar {
    background-color: #f0f0f0;
    color: black;
    min-height: 26px;
    padding: 0px 0px 0px 8px;
}

.seb-titlebar label {
    color: black;
    font-size: 11px;
    font-weight: 600;
    opacity: 1.0;
}

.seb-toolbar {
    background-color: #f0f0f0;
    color: black;
    min-height: 40px;
    padding: 0px 2px;
}

.seb-toolbar label.seb-toolbar-title {
    color: black;
    font-size: 13px;
    font-weight: 600;
}

.seb-hairline {
    background-color: #b0b0b0;
    min-height: 1px;
}

.seb-taskbar {
    background-color: #f0f0f0;
    color: black;
    min-height: 40px;
}

scrolledwindow.seb-app-scroll, scrolledwindow.seb-app-scroll viewport {
    background-color: #f0f0f0;
    background-image: none;
    border: none;
    box-shadow: none;
}

.seb-taskbar-active-bar {
    background-color: #1e90ff;
    min-height: 3px;
}

.seb-taskbar-inactive-bar {
    background-color: transparent;
    min-height: 3px;
}

button.seb-chrome-btn {
    background: #f0f0f0;
    background-image: none;
    color: black;
    border: 1px solid #f0f0f0;
    border-radius: 0px;
    padding: 0px 4px;
    box-shadow: none;
    text-shadow: none;
}

button.seb-chrome-btn:hover {
    background: #add8e6;
    background-image: none;
    border: 1px solid #1e90ff;
    color: black;
}

button.seb-chrome-btn:active {
    background: #add8e6;
    background-image: none;
    border: 2px solid #87ceeb;
    color: black;
}

button.seb-chrome-btn.popup-open {
    background: lightgray;
    border: 1px solid gray;
    color: black;
}

button.seb-chrome-btn:disabled {
    background: #f0f0f0;
    color: #696969;
    border: 1px solid #f0f0f0;
    opacity: 0.45;
}

button.seb-title-btn {
    background: #f0f0f0;
    background-image: none;
    color: black;
    border: 1px solid #f0f0f0;
    border-radius: 0px;
    min-width: 34px;
    min-height: 26px;
    padding: 0px;
    box-shadow: none;
}

button.seb-title-btn:hover {
    background: #add8e6;
    border: 1px solid #1e90ff;
}

button.seb-title-btn:active {
    background: #add8e6;
    border: 2px solid #87ceeb;
}

button.seb-title-btn:disabled {
    background: #f0f0f0;
    border: 1px solid #f0f0f0;
    opacity: 0.35;
}

entry,
entry.seb-url-entry {
    background-color: #f0f0f0;
    background-image: none;
    color: black;
    border: 1px solid gray;
    border-radius: 0px;
    padding: 3px 6px;
    min-height: 26px;
    box-shadow: none;
}

entry.readonly,
entry.seb-url-entry.readonly,
entry:disabled {
    background-color: lightgray;
    color: black;
    border: 1px solid gray;
}

entry:focus,
entry.seb-url-entry:focus {
    border: 1px solid #1e90ff;
    box-shadow: none;
    outline: none;
}

entry selection,
entry.seb-url-entry selection {
    background-color: #add8e6;
    color: black;
}

.seb-snap-preview {
    background-color: rgba(173, 216, 230, 0.47);
    border: 1px solid #1e90ff;
}

.seb-action-center {
    background-color: rgba(240, 240, 240, 0.95);
    border-right: 1px solid gray;
    color: black;
}

.seb-action-center label {
    color: black;
}

button.seb-ac-tile {
    background: lightgray;
    background-image: none;
    color: black;
    border: 1px solid gray;
    border-radius: 0px;
    min-height: 78px;
    padding: 4px;
    font-size: 10px;
    box-shadow: none;
}

button.seb-ac-tile:hover {
    background: #add8e6;
    border: 1px solid #1e90ff;
}

button.seb-ac-tile:active {
    background: #add8e6;
    border: 2px solid #87ceeb;
}

button.seb-ac-win-row {
    background: transparent;
    background-image: none;
    color: black;
    border: 1px solid transparent;
    border-radius: 0px;
    padding: 6px 10px;
    min-height: 36px;
    box-shadow: none;
}

button.seb-ac-win-row:hover {
    background: #add8e6;
    border: 1px solid #1e90ff;
}

.seb-switcher-overlay {
    background-color: #f0f0f0;
    border: 1px solid gray;
    border-radius: 8px;
    padding: 14px;
}

.seb-switcher-card {
    background-color: lightgray;
    border: 2px solid gray;
    border-radius: 4px;
    padding: 7px 8px 8px 8px;
}

.seb-switcher-card:hover {
    background-color: #add8e6;
    border: 2px solid #1e90ff;
}

.seb-switcher-card.selected {
    background-color: #add8e6;
    border: 2px solid #1e90ff;
}

.seb-switcher-thumb {
    background-color: #d8d8d8;
    border: 1px solid #b0b0b0;
    color: #696969;
}

.seb-switcher-card.selected .seb-switcher-thumb,
.seb-switcher-card:hover .seb-switcher-thumb {
    border: 1px solid #1e90ff;
}

popover.background.seb-popup,
.seb-popup {
    background-color: lightgray;
    background-image: none;
    border: 1px solid gray;
    border-radius: 0px;
    color: black;
    padding: 6px;
    box-shadow: none;
}

popover.background.seb-popup.seb-frame-popup,
.seb-popup.seb-frame-popup {
    background-color: #f0f0f0;
    border: 1px solid gray;
    border-radius: 0px;
    color: black;
    padding: 4px;
    box-shadow: none;
}

popover.background.seb-popup.seb-find-popup,
.seb-popup.seb-find-popup {
    padding: 6px;
}

.seb-popup label {
    color: black;
}

scrolledwindow scrollbar,
scrollbar {
    background-color: transparent;
    border: none;
}

scrollbar trough {
    background-color: #d8d8d8;
    border: none;
    min-width: 10px;
    min-height: 10px;
}

scrollbar slider {
    background-color: #b0b0b0;
    border-radius: 0px;
    border: none;
    min-width: 8px;
    min-height: 20px;
}

scrollbar slider:hover {
    background-color: gray;
}

scale trough {
    background-color: #d8d8d8;
    border: 1px solid gray;
    border-radius: 0px;
    min-height: 6px;
}

scale highlight {
    background-color: #1e90ff;
    border-radius: 0px;
}

scale slider {
    background-color: #f0f0f0;
    background-image: none;
    border: 1px solid gray;
    border-radius: 0px;
    box-shadow: none;
}

list,
listview,
treeview,
textview,
textview text {
    background-color: #f0f0f0;
    color: black;
    border-color: gray;
}

list row {
    background-color: #f0f0f0;
    color: black;
}

list row:hover,
list row:selected {
    background-color: #add8e6;
    color: black;
}

tooltip,
tooltip.background {
    background-color: #f0f0f0;
    background-image: none;
    border: 1px solid gray;
    border-radius: 0px;
    color: black;
    box-shadow: none;
}

tooltip label {
    color: black;
}

menu,
.menu,
.context-menu {
    background-color: #f0f0f0;
    background-image: none;
    border: 1px solid gray;
    border-radius: 0px;
    color: black;
    box-shadow: none;
}

menuitem {
    color: black;
}

menuitem:hover {
    background-color: #add8e6;
    color: black;
}

.seb-secondary-text {
    color: #696969;
}

.seb-clock-time {
    color: #696969;
    font-weight: 700;
}

.seb-clock-date {
    color: #696969;
}
"#;

pub fn install_light_theme() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_theme_name(Some("Adwaita"));
        settings.set_gtk_application_prefer_dark_theme(false);
        settings.set_gtk_dialogs_use_header(false);
    }
    let provider = gtk::CssProvider::new();
    let _ = provider.load_from_data(SEB_LIGHT_CSS.as_bytes());
    if let Some(screen) = gdk::Screen::default() {
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
}
