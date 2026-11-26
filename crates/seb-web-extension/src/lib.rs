use glib::translate::FromGlibPtrBorrow;
use libc::{c_char, c_int, c_uint, c_ulong, c_void};
use seb_config::{calculate_browser_exam_key_hash, calculate_configuration_key_hash};
use seb_filter::{FilterConfig, FilterDecision, RequestFilter, ResourceType};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::sync::Arc;
use url::Url;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebExtensionInitConfig {
    pub send_configuration_key: bool,
    pub send_browser_exam_key: bool,
    pub configuration_key: String,
    pub browser_exam_key: String,
    pub accept_language: String,
    pub filter: FilterConfig,
}

struct ExtensionState {
    cfg: WebExtensionInitConfig,
    filter: RequestFilter,
}

// Symbols provided by the host WebKitWebProcess (libwebkit2gtk-4.1 + libsoup-3.0 + libgobject-2.0).
#[link(name = "webkit2gtk-4.1")]
#[link(name = "soup-3.0")]
#[link(name = "gobject-2.0")]
extern "C" {
    fn g_signal_connect_data(
        instance: *mut c_void,
        detailed_signal: *const c_char,
        c_handler: *const c_void,
        data: *mut c_void,
        destroy_data: *const c_void,
        connect_flags: c_uint,
    ) -> c_ulong;

    fn webkit_web_page_get_uri(web_page: *mut c_void) -> *const c_char;
    fn webkit_web_page_get_main_frame(web_page: *mut c_void) -> *mut c_void;
    fn webkit_frame_get_uri(frame: *mut c_void) -> *const c_char;
    fn webkit_uri_request_get_uri(request: *mut c_void) -> *const c_char;
    fn webkit_uri_request_get_http_headers(request: *mut c_void) -> *mut c_void;
    fn soup_message_headers_get_one(
        hdrs: *mut c_void,
        name: *const c_char,
    ) -> *const c_char;
    fn soup_message_headers_replace(
        hdrs: *mut c_void,
        name: *const c_char,
        value: *const c_char,
    );
}

fn same_host(first_party: &str, request_uri: &str) -> bool {
    let (Ok(a), Ok(b)) = (Url::parse(first_party), Url::parse(request_uri)) else {
        return false;
    };
    match (a.host_str(), b.host_str()) {
        (Some(ha), Some(hb)) => ha.eq_ignore_ascii_case(hb),
        _ => false,
    }
}

unsafe fn is_main_frame_request(
    page: *mut c_void,
    request: *mut c_void,
    _redirect_response: *mut c_void,
    uri: &str,
    page_uri: &str,
) -> bool {
    if page_uri.is_empty() || page_uri == uri {
        return true;
    }
    // Check Fetch Metadata first: Sec-Fetch-Dest is authoritative when present
    // so iframes / XHR / redirected subresources are never misclassified as MainFrame.
    let headers = webkit_uri_request_get_http_headers(request);
    if !headers.is_null() {
        let dest_ptr =
            soup_message_headers_get_one(headers, b"Sec-Fetch-Dest\0".as_ptr() as *const c_char);
        if !dest_ptr.is_null() {
            if let Ok(dest) = CStr::from_ptr(dest_ptr).to_str() {
                return dest.eq_ignore_ascii_case("document");
            }
        }
    }
    if !page.is_null() {
        let main_frame = webkit_web_page_get_main_frame(page);
        if !main_frame.is_null() {
            let mf_ptr = webkit_frame_get_uri(main_frame);
            if !mf_ptr.is_null() {
                if let Ok(mf_uri) = CStr::from_ptr(mf_ptr).to_str() {
                    if mf_uri.is_empty() || mf_uri == uri {
                        return true;
                    }
                }
            }
        }
    }
    if !headers.is_null() {
        let accept_ptr =
            soup_message_headers_get_one(headers, b"Accept\0".as_ptr() as *const c_char);
        if !accept_ptr.is_null() {
            if let Ok(accept) = CStr::from_ptr(accept_ptr).to_str() {
                if accept.contains("text/html") {
                    return true;
                }
            }
        }
    }
    false
}

unsafe extern "C" fn on_send_request(
    page: *mut c_void,
    request: *mut c_void,
    redirect_response: *mut c_void,
    user_data: *mut c_void,
) -> c_int {
    if request.is_null() || user_data.is_null() {
        return 0;
    }
    let state = &*(user_data as *const ExtensionState);

    let uri_ptr = webkit_uri_request_get_uri(request);
    if uri_ptr.is_null() {
        return 0;
    }
    let Ok(uri) = CStr::from_ptr(uri_ptr).to_str() else {
        return 0;
    };

    // Unconditionally block mailto: (byte-safe check across FFI boundary)
    if uri
        .as_bytes()
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"mailto:"))
    {
        return 1;
    }

    let page_uri_ptr = if page.is_null() {
        std::ptr::null()
    } else {
        webkit_web_page_get_uri(page)
    };
    let page_uri = if page_uri_ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(page_uri_ptr).to_str().unwrap_or("")
    };

    let is_main = is_main_frame_request(page, request, redirect_response, uri, page_uri);
    let rtype = if is_main {
        ResourceType::MainFrame
    } else {
        ResourceType::Other
    };

    if state.filter.evaluate(uri, rtype) == FilterDecision::Block {
        return 1;
    }

    if uri.starts_with("http://") || uri.starts_with("https://") {
        let headers = webkit_uri_request_get_http_headers(request);
        if !headers.is_null() {
            // CRITICAL SECURITY PARITY (request_interceptor.cpp:34-50):
            // Append SEB key hashes ONLY on MainFrame or same-host subresources —
            // NEVER leak exam hashes to third-party hosts!
            let append_seb_headers = is_main || same_host(page_uri, uri);
            if append_seb_headers {
                if state.cfg.send_configuration_key && !state.cfg.configuration_key.is_empty() {
                    let hash = calculate_configuration_key_hash(&state.cfg.configuration_key, uri);
                    if let Ok(val) = CString::new(hash) {
                        soup_message_headers_replace(
                            headers,
                            b"X-SafeExamBrowser-ConfigKeyHash\0".as_ptr() as *const c_char,
                            val.as_ptr(),
                        );
                    }
                }
                if state.cfg.send_browser_exam_key && !state.cfg.browser_exam_key.is_empty() {
                    let hash = calculate_browser_exam_key_hash(&state.cfg.browser_exam_key, uri);
                    if let Ok(val) = CString::new(hash) {
                        soup_message_headers_replace(
                            headers,
                            b"X-SafeExamBrowser-RequestHash\0".as_ptr() as *const c_char,
                            val.as_ptr(),
                        );
                    }
                }
            }
            if !state.cfg.accept_language.is_empty() {
                if let Ok(val) = CString::new(state.cfg.accept_language.as_str()) {
                    soup_message_headers_replace(
                        headers,
                        b"Accept-Language\0".as_ptr() as *const c_char,
                        val.as_ptr(),
                    );
                }
            }
        }
    }

    0
}

unsafe extern "C" fn on_page_created(
    _extension: *mut c_void,
    page: *mut c_void,
    user_data: *mut c_void,
) {
    if page.is_null() || user_data.is_null() {
        return;
    }
    g_signal_connect_data(
        page,
        b"send-request\0".as_ptr() as *const c_char,
        on_send_request as *const c_void,
        user_data,
        std::ptr::null(),
        0,
    );
}

#[no_mangle]
pub unsafe extern "C" fn webkit_web_extension_initialize_with_user_data(
    extension: *mut c_void,
    user_data: *mut glib::ffi::GVariant,
) {
    if extension.is_null() {
        return;
    }
    let cfg: WebExtensionInitConfig = if user_data.is_null() {
        WebExtensionInitConfig::default()
    } else {
        let variant = glib::Variant::from_glib_borrow(user_data);
        variant
            .str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    };

    let state = Arc::new(ExtensionState {
        filter: RequestFilter::new(&cfg.filter),
        cfg,
    });
    let raw_state = Arc::into_raw(state) as *mut c_void;

    g_signal_connect_data(
        extension,
        b"page-created\0".as_ptr() as *const c_char,
        on_page_created as *const c_void,
        raw_state,
        std::ptr::null(),
        0,
    );
}
