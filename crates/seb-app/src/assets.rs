use gdk_pixbuf::prelude::PixbufLoaderExt;
use gdk_pixbuf::{Pixbuf, PixbufLoader};

pub const ICON_SEB_PNG: &[u8] = include_bytes!("../../../assets/icons/safe-exam-browser-64.png");
pub const ICON_ABOUT_SVG: &[u8] = include_bytes!("../../../assets/taskbar/about.svg");
pub const ICON_AUDIO_100_SVG: &[u8] = include_bytes!("../../../assets/taskbar/audio_100.svg");
pub const ICON_AUDIO_66_SVG: &[u8] = include_bytes!("../../../assets/taskbar/audio_66.svg");
pub const ICON_AUDIO_33_SVG: &[u8] = include_bytes!("../../../assets/taskbar/audio_33.svg");
pub const ICON_AUDIO_MUTED_SVG: &[u8] = include_bytes!("../../../assets/taskbar/audio_muted.svg");
pub const ICON_AUDIO_NONE_SVG: &[u8] = include_bytes!("../../../assets/taskbar/audio_no_device.svg");
pub const ICON_BACK_SVG: &[u8] = include_bytes!("../../../assets/taskbar/back.svg");
pub const ICON_BATTERY_SVG: &[u8] = include_bytes!("../../../assets/taskbar/battery.svg");
pub const ICON_FORWARD_SVG: &[u8] = include_bytes!("../../../assets/taskbar/forward.svg");
pub const ICON_HAND_LOWERED_SVG: &[u8] = include_bytes!("../../../assets/taskbar/hand_lowered.svg");
pub const ICON_HAND_RAISED_SVG: &[u8] = include_bytes!("../../../assets/taskbar/hand_raised.svg");
pub const ICON_HOME_SVG: &[u8] = include_bytes!("../../../assets/taskbar/home.svg");
pub const ICON_LOG_SVG: &[u8] = include_bytes!("../../../assets/taskbar/log.svg");
pub const ICON_NETWORK_WIRED_SVG: &[u8] = include_bytes!("../../../assets/taskbar/network_wired.svg");
pub const ICON_PROCTORING_SVG: &[u8] = include_bytes!("../../../assets/taskbar/proctoring.svg");
pub const ICON_RELOAD_SVG: &[u8] = include_bytes!("../../../assets/taskbar/reload.svg");
pub const ICON_SHUTDOWN_SVG: &[u8] = include_bytes!("../../../assets/taskbar/shutdown.svg");
pub const ICON_VERIFICATOR_SVG: &[u8] = include_bytes!("../../../assets/taskbar/verificator.svg");
pub const ICON_WIFI_0_SVG: &[u8] = include_bytes!("../../../assets/taskbar/wifi_0.svg");
pub const ICON_WIFI_33_SVG: &[u8] = include_bytes!("../../../assets/taskbar/wifi_33.svg");
pub const ICON_WIFI_66_SVG: &[u8] = include_bytes!("../../../assets/taskbar/wifi_66.svg");
pub const ICON_WIFI_100_SVG: &[u8] = include_bytes!("../../../assets/taskbar/wifi_100.svg");

pub const BLOCKED_PAGE_HTML: &str = r#"<!DOCTYPE html>
<html style="height: 100%; width: 100%">
<head><meta charset="utf-8" /><title>Blocked</title></head>
<body style="background-color: lightgray; display: table; font-family: sans-serif; height: 98%; text-align: center; width: 99%; color: black;">
  <div style="display: table-cell; vertical-align: middle">
    <p style="font-weight: bold">Access to this resource is not allowed according to the current SEB configuration.</p>
    <p style="color: #696969; font-size: 13px;">%%URL%%</p>
    <button onclick="window.history.back()" style="cursor: pointer; padding: 6px 14px;">&#x2B60; Back</button>
  </div>
</body>
</html>"#;

#[allow(dead_code)]
pub const BLOCKED_CONTENT_HTML: &str = r#"<!DOCTYPE html>
<html style="height: 100%; width: 100%">
<head><meta charset="utf-8" /><title>Blocked Content</title></head>
<body style="background-color: lightgray; display: table; font-family: sans-serif; height: 98%; text-align: center; width: 99%; color: black;">
  <div style="display: table-cell; vertical-align: middle">
    <p style="font-weight: bold">Blocked content</p>
  </div>
</body>
</html>"#;

pub const TLS_ERROR_HTML: &str = r#"<!DOCTYPE html>
<html style="height: 100%; width: 100%">
<head><meta charset="utf-8" /><title>Certificate Error</title></head>
<body style="background-color: lightgray; display: table; font-family: sans-serif; height: 98%; text-align: center; width: 99%; color: black;">
  <div style="display: table-cell; vertical-align: middle">
    <p style="font-weight: bold">SSL/TLS certificate verification failed for this resource.</p>
    <p style="color: #696969; font-size: 13px;">%%URL%%</p>
    <button onclick="window.history.back()" style="cursor: pointer; padding: 6px 14px;">&#x2B60; Back</button>
  </div>
</body>
</html>"#;

pub const SEB_API_JS_TEMPLATE: &str = r#"(function () {
  'use strict';
  var callbacks = Object.create(null);
  var nextRequestId = 0;

  var updateKeys = function (callback) {
    var requestId = String(++nextRequestId);
    if (typeof callback === 'function') {
      callbacks[requestId] = callback;
    }
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.sebSecurity) {
        window.webkit.messageHandlers.sebSecurity.postMessage(JSON.stringify({
          id: requestId,
          url: window.location.href
        }));
      }
    } catch (e) {}
  };

  window.__sebApplyKeys = function (id, bek, ck) {
    window.SafeExamBrowser.security.browserExamKey = bek || '';
    window.SafeExamBrowser.security.configKey = ck || '';
    var cb = callbacks[id];
    delete callbacks[id];
    if (typeof cb === 'function') {
      try { cb(); } catch (e) {}
    }
  };

  window.SafeExamBrowser = {
    version: '%%SEB_VERSION%%',
    security: {
      browserExamKey: '%%INITIAL_BEK%%',
      configKey: '%%INITIAL_CK%%',
      updateKeys: updateKeys
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { updateKeys(); }, { once: true });
  } else {
    updateKeys();
  }
  window.addEventListener('popstate', function () { updateKeys(); });
}());"#;

pub const PAGE_ZOOM_BLOCK_JS: &str = r#"(function () {
  'use strict';
  document.addEventListener('wheel', function (e) {
    if (e.ctrlKey) {
      e.preventDefault();
      e.stopPropagation();
    }
  }, { capture: true, passive: false });
}());"#;

pub const CLIPBOARD_BLOCK_JS: &str = r#"(function () {
  'use strict';
  ['copy', 'cut', 'paste'].forEach(function (ev) {
    document.addEventListener(ev, function (e) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }, true);
  });
}());"#;

pub const CLIPBOARD_ISOLATED_JS: &str = r#"(function () {
  'use strict';
  var state = { id: '', content: '' };

  function encodeBase64(str) {
    try {
      var bytes = new TextEncoder().encode(str);
      var bin = '';
      for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
      return btoa(bin);
    } catch (e) { return ''; }
  }

  function decodeBase64(b64) {
    try {
      var bin = atob(b64);
      var bytes = new Uint8Array(bin.length);
      for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      return new TextDecoder().decode(bytes);
    } catch (e) { return ''; }
  }

  function isPasswordField(el) {
    return el && el.tagName === 'INPUT' && (el.type || '').toLowerCase() === 'password';
  }

  function postUpdate(text) {
    var id = String(Date.now()) + '-' + Math.random().toString(36).slice(2, 8);
    state.id = id;
    state.content = text;
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.sebClipboard) {
        window.webkit.messageHandlers.sebClipboard.postMessage(JSON.stringify({
          id: id,
          content: encodeBase64(text)
        }));
      }
    } catch (e) {}
  }

  function performCopyOrCut(isCut) {
    var el = document.activeElement;
    if (isPasswordField(el)) return;
    if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) {
      var start = el.selectionStart || 0;
      var end = el.selectionEnd || 0;
      if (end > start) {
        var text = el.value.slice(start, end);
        postUpdate(text);
        if (isCut && !el.readOnly && !el.disabled) {
          el.setRangeText('', start, end, 'start');
          el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteByCut' }));
        }
      }
      return;
    }
    var sel = window.getSelection();
    if (sel && sel.rangeCount > 0 && !sel.isCollapsed) {
      var txt = sel.toString();
      postUpdate(txt);
      if (isCut && (document.designMode === 'on' || (el && el.isContentEditable))) {
        sel.deleteFromDocument();
        if (el) el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteByCut' }));
      }
    }
  }

  function performPaste() {
    var text = state.content || '';
    if (!text) return;
    var el = document.activeElement;
    if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) {
      if (el.readOnly || el.disabled) return;
      var start = el.selectionStart || 0;
      var end = el.selectionEnd || 0;
      el.setRangeText(text, start, end, 'end');
      el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertFromPaste' }));
      return;
    }
    if (document.designMode === 'on' || (el && el.isContentEditable)) {
      var sel = window.getSelection();
      if (sel && sel.rangeCount > 0) {
        var range = sel.getRangeAt(0);
        range.deleteContents();
        var node = document.createTextNode(text);
        range.insertNode(node);
        range.setStartAfter(node);
        range.collapse(true);
        sel.removeAllRanges();
        sel.addRange(range);
        if (el) el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertFromPaste' }));
      }
    }
  }

  document.addEventListener('copy', function (e) {
    if (!e.isTrusted) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    performCopyOrCut(false);
  }, true);

  document.addEventListener('cut', function (e) {
    if (!e.isTrusted) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    performCopyOrCut(true);
  }, true);

  document.addEventListener('paste', function (e) {
    if (!e.isTrusted) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    performPaste();
  }, true);

  window.__sebClipboard = {
    update: function (id, b64) {
      if (id && id !== state.id) {
        state.id = id;
        state.content = decodeBase64(b64 || '');
      }
    },
    exec: function (cmd) {
      if (cmd === 'copy') performCopyOrCut(false);
      else if (cmd === 'cut') performCopyOrCut(true);
      else if (cmd === 'paste') performPaste();
    }
  };
}());"#;

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn pixbuf_from_bytes(bytes: &[u8], width: i32, height: i32) -> Option<Pixbuf> {
    let loader = PixbufLoader::new();
    loader.set_size(width, height);
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

pub fn image_from_bytes(bytes: &[u8], width: i32, height: i32) -> gtk::Image {
    match pixbuf_from_bytes(bytes, width, height) {
        Some(pb) => gtk::Image::from_pixbuf(Some(&pb)),
        None => gtk::Image::new(),
    }
}

/// Dynamic battery icon matching C++ `battery_control.cpp`:
/// proportional fill with Charging (#1e88e5 + yellow bolt), Critical (#d32f2f),
/// Low (#ed6c02), Normal (#2e7d32).
pub fn dynamic_battery_image(
    percentage: i32,
    charging: bool,
    critical_thresh: f64,
    low_thresh: f64,
    size: i32,
) -> gtk::Image {
    let pct = percentage.clamp(0, 100);
    let frac = pct as f64 / 100.0;
    let fill_color = if charging {
        "#1e88e5"
    } else if frac <= critical_thresh {
        "#d32f2f"
    } else if frac <= low_thresh {
        "#ed6c02"
    } else {
        "#2e7d32"
    };
    let fill_w = (26.0 * frac).max(if pct > 0 { 1.5 } else { 0.0 });
    let bolt = if charging {
        r##"<polygon points="22,12 16,23 21,23 18,32 27,20 22,20" fill="#ffd54f" stroke="#1f1f1f" stroke-width="1.2" stroke-linejoin="round"/>"##
    } else {
        ""
    };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 44 44">
  <rect x="4" y="12" width="32" height="20" rx="2.5" fill="#ffffff" stroke="#1f1f1f" stroke-width="2.4"/>
  <rect x="7" y="15" width="{fill_w:.1}" height="14" rx="1" fill="{fill_color}"/>
  <rect x="36" y="17" width="4" height="10" rx="1" fill="#1f1f1f"/>
  {bolt}
</svg>"##
    );
    image_from_bytes(svg.as_bytes(), size, size)
}

pub fn minimize_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <line x1="6" y1="15.5" x2="18" y2="15.5" stroke="black" stroke-width="1.6"/>
</svg>"##
}

pub fn maximize_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <rect x="5.5" y="5.5" width="13" height="13" fill="none" stroke="black" stroke-width="1.6"/>
</svg>"##
}

pub fn restore_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <path d="M8.5 7.5 V4.5 H19.5 V15.5 H16.5" fill="none" stroke="black" stroke-width="1.6"/>
  <rect x="4.5" y="8.5" width="11" height="11" fill="none" stroke="black" stroke-width="1.6"/>
</svg>"##
}

pub fn close_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <path d="M6 6 L18 18 M18 6 L6 18" stroke="black" stroke-width="1.8" stroke-linecap="round"/>
</svg>"##
}

pub fn zoom_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <circle cx="10" cy="10" r="6" fill="none" stroke="black" stroke-width="1.8"/>
  <line x1="14.5" y1="14.5" x2="20" y2="20" stroke="black" stroke-width="1.8" stroke-linecap="round"/>
  <line x1="10" y1="7" x2="10" y2="13" stroke="black" stroke-width="1.5"/>
  <line x1="7" y1="10" x2="13" y2="10" stroke="black" stroke-width="1.5"/>
</svg>"##
}

pub fn search_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <circle cx="10" cy="10" r="6" fill="none" stroke="black" stroke-width="1.8"/>
  <line x1="14.5" y1="14.5" x2="20" y2="20" stroke="black" stroke-width="1.8" stroke-linecap="round"/>
</svg>"##
}

pub fn keyboard_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 44 44">
  <rect x="4" y="11" width="36" height="22" rx="3" fill="none" stroke="black" stroke-width="2.4"/>
  <rect x="8" y="15" width="4" height="3" fill="black"/>
  <rect x="14" y="15" width="4" height="3" fill="black"/>
  <rect x="20" y="15" width="4" height="3" fill="black"/>
  <rect x="26" y="15" width="4" height="3" fill="black"/>
  <rect x="32" y="15" width="4" height="3" fill="black"/>
  <rect x="12" y="26" width="20" height="3" fill="black"/>
</svg>"##
}

pub fn clock_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 44 44">
  <circle cx="22" cy="22" r="16" fill="none" stroke="black" stroke-width="2.8"/>
  <line x1="22" y1="22" x2="22" y2="13" stroke="black" stroke-width="2.8" stroke-linecap="round"/>
  <line x1="22" y1="22" x2="29" y2="25" stroke="black" stroke-width="2.8" stroke-linecap="round"/>
</svg>"##
}

pub fn external_app_icon_svg() -> &'static [u8] {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
  <rect x="3" y="4" width="18" height="16" rx="2" fill="none" stroke="black" stroke-width="1.8"/>
  <line x1="3" y1="8.5" x2="21" y2="8.5" stroke="black" stroke-width="1.5"/>
</svg>"##
}
