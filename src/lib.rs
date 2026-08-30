//! A Rust port of nginx's `ngx_http_not_modified_filter_module`.
//!
//! The module implements conditional requests: it inspects `If-Modified-Since`,
//! `If-Unmodified-Since`, `If-Match` and `If-None-Match` against the response's
//! `Last-Modified` and `ETag`, and turns a `200 OK` into `304 Not Modified` or
//! `412 Precondition Failed` where the conditions call for it.
//!
//! It exists mainly as a worked example of writing a *header filter* with
//! [ngx-rust], which currently provides no wrapper for the filter chain.  The
//! structure deliberately follows the C original line for line so the two can be
//! read side by side.
//!
//! [ngx-rust]: https://github.com/nginx/ngx-rust
use core::ptr;

use ngx::core::Status;
use ngx::ffi::{
    ngx_conf_t, ngx_http_filter_finalize_request, ngx_http_module_t,
    ngx_http_output_header_filter_pt, ngx_http_request_t, ngx_http_top_header_filter, ngx_int_t,
    ngx_module_t, ngx_parse_http_time, ngx_str_t, ngx_table_elt_t, ngx_uint_t, NGX_HTTP_IMS_EXACT,
    NGX_HTTP_IMS_OFF, NGX_HTTP_MODULE, NGX_HTTP_NOT_MODIFIED, NGX_HTTP_PRECONDITION_FAILED,
};
use ngx::http::{HttpModule, HttpModuleLocationConf, NgxHttpCoreModule};
use ngx::ngx_log_debug;
#[cfg(feature = "export-modules")]
use ngx::ngx_modules;

/// Entity-tag list matching. Pure byte slicing, no nginx types.
mod etag;
use etag::etag_list_contains;

struct Module;

/// Holds the filter that was at the top of the chain when we installed ourselves.
///
/// Every header filter has to call the next one, or the response stops here.
static mut NEXT_HEADER_FILTER: ngx_http_output_header_filter_pt = None;

impl HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        // SAFETY: the module is a `static mut` that nginx only writes during
        // initialization; taking a shared reference to it is the established
        // pattern in ngx-rust's own examples.
        unsafe { &*ptr::addr_of!(ngx_http_not_modified_filter_rs_module) }
    }

    /// Installs the header filter.
    ///
    /// This is `ngx_http_not_modified_filter_init` in the C original.  Filters
    /// form a singly linked list of function pointers built during
    /// postconfiguration: each module saves the current head and puts itself in
    /// front, so the module configured *last* runs *first*.
    unsafe extern "C" fn postconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
        // SAFETY: postconfiguration runs single-threaded during configuration
        // parsing, before any worker exists, so there is no concurrent access
        // to these globals.
        unsafe {
            NEXT_HEADER_FILTER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(ngx_http_not_modified_header_filter);
        }

        Status::NGX_OK.into()
    }
}

static NGX_HTTP_NOT_MODIFIED_FILTER_RS_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: None,
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: None,
    merge_loc_conf: None,
};

// Built as a dynamic module, nginx finds the module through the `ngx_modules`
// table this macro generates. Built into nginx statically, it looks the symbol
// up by name instead, so the mangling has to be turned off.
#[cfg(feature = "export-modules")]
ngx_modules!(ngx_http_not_modified_filter_rs_module);

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), no_mangle)]
pub static mut ngx_http_not_modified_filter_rs_module: ngx_module_t = ngx_module_t {
    ctx: &raw const NGX_HTTP_NOT_MODIFIED_FILTER_RS_MODULE_CTX as _,
    commands: ptr::null_mut(),
    type_: NGX_HTTP_MODULE as ngx_uint_t,
    ..ngx_module_t::default()
};

/// The filter itself. Mirrors `ngx_http_not_modified_header_filter`.
///
/// # Safety
///
/// Called by nginx with a valid request pointer.
unsafe extern "C" fn ngx_http_not_modified_header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
    // SAFETY: nginx passes a valid, uniquely borrowed request to header filters.
    let request = unsafe { &mut *r };

    // SAFETY: a request being filtered always has a live connection.
    ngx_log_debug!(
        unsafe { (*request.connection).log },
        "rust not_modified: entry status:{} main:{} disabled:{}",
        request.headers_out.status,
        ptr::eq(r, request.main),
        request.disable_not_modified()
    );

    // Only main requests with a plain 200 are candidates, and a handler may opt
    // out by setting `disable_not_modified`.
    if request.headers_out.status != ngx::ffi::NGX_HTTP_OK as ngx_uint_t
        || !ptr::eq(r, request.main)
        || request.disable_not_modified() != 0
    {
        return unsafe { next_filter(r) };
    }

    // If-Unmodified-Since: the resource must NOT have changed since the given
    // date, otherwise the request is refused outright.
    if !request.headers_in.if_unmodified_since.is_null() && !unsafe { test_if_unmodified(request) }
    {
        // SAFETY: `r` is valid; a null body means "no response body".
        return unsafe {
            ngx_http_filter_finalize_request(
                r,
                ptr::null_mut(),
                NGX_HTTP_PRECONDITION_FAILED as ngx_int_t,
            )
        };
    }

    // If-Match: the entity tag must match. Strong comparison.
    if !request.headers_in.if_match.is_null()
        && !unsafe { test_if_match(request, request.headers_in.if_match, false) }
    {
        return unsafe {
            ngx_http_filter_finalize_request(
                r,
                ptr::null_mut(),
                NGX_HTTP_PRECONDITION_FAILED as ngx_int_t,
            )
        };
    }

    if !request.headers_in.if_modified_since.is_null()
        || !request.headers_in.if_none_match.is_null()
    {
        // Either header alone is enough to prove the entity *did* change, in
        // which case the response is sent as-is.
        if !request.headers_in.if_modified_since.is_null() && unsafe { test_if_modified(request) } {
            return unsafe { next_filter(r) };
        }

        // If-None-Match uses weak comparison.
        if !request.headers_in.if_none_match.is_null()
            && !unsafe { test_if_match(request, request.headers_in.if_none_match, true) }
        {
            return unsafe { next_filter(r) };
        }

        // Not modified: strip the response down to a bare 304.
        request.headers_out.status = NGX_HTTP_NOT_MODIFIED as ngx_uint_t;
        request.headers_out.status_line.len = 0;
        request.headers_out.content_type.len = 0;

        clear_content_length(request);
        clear_accept_ranges(request);

        if !request.headers_out.content_encoding.is_null() {
            // SAFETY: checked non-null just above.
            unsafe { (*request.headers_out.content_encoding).hash = 0 };
            request.headers_out.content_encoding = ptr::null_mut();
        }

        return unsafe { next_filter(r) };
    }

    unsafe { next_filter(r) }
}

/// Hands the request to the next filter in the chain.
///
/// # Safety
///
/// `r` must be a valid request pointer.
unsafe fn next_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
    // SAFETY: `NEXT_HEADER_FILTER` is written once during postconfiguration and
    // only read afterwards. nginx always has a terminating filter, so the
    // Option is `Some` by the time any request is served.
    match unsafe { NEXT_HEADER_FILTER } {
        Some(next) => unsafe { next(r) },
        None => Status::NGX_ERROR.into(),
    }
}

/// `ngx_http_test_if_unmodified`: true when the entity has *not* changed since
/// the client's date, i.e. the precondition holds.
///
/// # Safety
///
/// `if_unmodified_since` must be non-null.
unsafe fn test_if_unmodified(request: &ngx_http_request_t) -> bool {
    if request.headers_out.last_modified_time == -1 {
        return false;
    }

    // SAFETY: the caller checked the header is present.
    let value = unsafe { &(*request.headers_in.if_unmodified_since).value };
    let iums = unsafe { ngx_parse_http_time(value.data, value.len) };

    // SAFETY: a request being filtered always has a live connection.
    ngx_log_debug!(
        unsafe { (*request.connection).log },
        "rust not_modified: iums:{} lm:{}",
        iums,
        request.headers_out.last_modified_time
    );

    iums >= request.headers_out.last_modified_time
}

/// `ngx_http_test_if_modified`: true when the entity *has* changed, so the full
/// response should be sent.
///
/// # Safety
///
/// `if_modified_since` must be non-null.
unsafe fn test_if_modified(request: &ngx_http_request_t) -> bool {
    if request.headers_out.last_modified_time == -1 {
        return true;
    }

    let Some(clcf) = NgxHttpCoreModule::location_conf(request) else {
        return true;
    };

    if clcf.if_modified_since == NGX_HTTP_IMS_OFF as ngx_uint_t {
        return true;
    }

    // SAFETY: the caller checked the header is present.
    let value = unsafe { &(*request.headers_in.if_modified_since).value };
    let ims = unsafe { ngx_parse_http_time(value.data, value.len) };

    // SAFETY: a request being filtered always has a live connection.
    ngx_log_debug!(
        unsafe { (*request.connection).log },
        "rust not_modified: ims:{} lm:{}",
        ims,
        request.headers_out.last_modified_time
    );

    if ims == request.headers_out.last_modified_time {
        return false;
    }

    clcf.if_modified_since == NGX_HTTP_IMS_EXACT as ngx_uint_t
        || ims < request.headers_out.last_modified_time
}

/// `ngx_http_test_if_match`: does the response's ETag appear in the header's
/// comma-separated list?
///
/// With `weak` set, a leading `W/` is stripped from both sides before
/// comparing, which is what `If-None-Match` calls for.
///
/// # Safety
///
/// `header` must be a valid, non-null `ngx_table_elt_t`.
unsafe fn test_if_match(
    request: &ngx_http_request_t,
    header: *mut ngx_table_elt_t,
    weak: bool,
) -> bool {
    // SAFETY: the caller guarantees `header` is valid.
    let list: &ngx_str_t = unsafe { &(*header).value };
    let list_bytes = list.as_bytes();

    if list_bytes == b"*" {
        return true;
    }

    if request.headers_out.etag.is_null() {
        return false;
    }

    // SAFETY: checked non-null just above.
    let etag_value: &ngx_str_t = unsafe { &(*request.headers_out.etag).value };
    let mut etag = etag_value.as_bytes();

    // SAFETY: a request being filtered always has a live connection.
    ngx_log_debug!(
        unsafe { (*request.connection).log },
        "rust not_modified: im:\"{}\" etag:{}",
        String::from_utf8_lossy(list_bytes),
        String::from_utf8_lossy(etag)
    );

    if weak && etag.len() > 2 && etag.starts_with(b"W/") {
        etag = &etag[2..];
    }

    etag_list_contains(list_bytes, etag, weak)
}

/// The `ngx_http_clear_content_length` macro.
fn clear_content_length(request: &mut ngx_http_request_t) {
    request.headers_out.content_length_n = -1;

    if !request.headers_out.content_length.is_null() {
        // SAFETY: checked non-null just above.
        unsafe { (*request.headers_out.content_length).hash = 0 };
        request.headers_out.content_length = ptr::null_mut();
    }
}

/// The `ngx_http_clear_accept_ranges` macro.
fn clear_accept_ranges(request: &mut ngx_http_request_t) {
    request.set_allow_ranges(0);

    if !request.headers_out.accept_ranges.is_null() {
        // SAFETY: checked non-null just above.
        unsafe { (*request.headers_out.accept_ranges).hash = 0 };
        request.headers_out.accept_ranges = ptr::null_mut();
    }
}
