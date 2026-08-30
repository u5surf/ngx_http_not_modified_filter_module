# ngx_http_not_modified_filter_module (Rust)

A port of nginx's [`ngx_http_not_modified_filter_module`](https://github.com/nginx/nginx/blob/017cf98dcce217946572a896f0992370475e189f/src/http/modules/ngx_http_not_modified_filter_module.c) to Rust, built on [ngx-rust](https://github.com/nginx/ngx-rust).

The module implements conditional requests: it compares `If-Modified-Since`, `If-Unmodified-Since`, `If-Match` and `If-None-Match` against the response's `Last-Modified` and `ETag`, and turns a `200 OK` into `304 Not Modified` or `412 Precondition Failed` where the conditions call for it.

## Why

ngx-rust has no wrapper for the filter chain — no `HeaderFilter` trait, no equivalent of `add_phase_handler`, and not a single filter among its examples. This port exists to find out what writing one actually involves.

It is a study exercise, not something to run in production: nginx already ships this filter, and you cannot turn the built-in one off.

The structure follows the C original closely so the two can be read side by side.

## What writing a header filter takes

Filters are a singly linked list of function pointers. Each module saves the current head during `postconfiguration` and puts itself in front, so **the module initialized last runs first**.

```rust
static mut NEXT_HEADER_FILTER: ngx_http_output_header_filter_pt = None;

impl HttpModule for Module {
    unsafe extern "C" fn postconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
        unsafe {
            NEXT_HEADER_FILTER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(ngx_http_not_modified_header_filter);
        }
        Status::NGX_OK.into()
    }
}
```

Everything after that is ordinary FFI work: `&mut *r` to get at the request, null checks on `headers_in` fields, and a call to `NEXT_HEADER_FILTER` on every path out. Nothing in ngx-rust helps, but nothing gets in the way either.

## Build

The crate needs to be built against the same nginx it will run in.

### As a dynamic module

```sh
NGINX_BUILD_DIR=/path/to/nginx/objs \
  cargo build --no-default-features --features export-modules
```

Then load it. The file is `.dylib` on macOS and `.so` on Linux:

```nginx
load_module /path/to/target/debug/libngx_http_not_modified_filter_module.dylib;
```

Without `NGINX_BUILD_DIR`, the `vendored` feature builds its own nginx (1.28.x for ngx 0.5.0), and loading the result into a different nginx fails with `module ... version 1028003 instead of 1030004`.

### Built into nginx

```sh
cd /path/to/nginx
auto/configure --with-compat --with-debug --add-module=/path/to/this/repo
make
```

See [the ordering caveat](#a-statically-linked-filter-cannot-run-first) before doing this.

## Tests

The entity-tag list matching is pure byte slicing and is tested directly:

```sh
cargo test
```

```
running 6 tests
test tests::a_prefix_is_not_a_match ... ok
test tests::empty_list_matches_nothing ... ok
test tests::matches_inside_a_list ... ok
test tests::tolerates_padding_between_entries ... ok
test tests::matches_a_single_tag ... ok
test tests::weak_prefix_is_stripped_from_list_entries ... ok
```

The tests live in `tests/etag_list.rs` and `include!` the source rather than linking the library. A test harness built from the library itself cannot start — it would need nginx's symbols, which only exist inside the nginx binary:

```
dyld[92367]: symbol not found in flat namespace '_ngx_http_core_module'
```

That is also why `[lib] test = false` is set in `Cargo.toml`.

## Verified behaviour

Loaded as a dynamic module into nginx 1.30.4 on macOS, ahead of the built-in filter, serving a static file with `etag on`:

| request | expected | actual |
| --- | --- | --- |
| plain `GET` | 200 | 200 |
| `If-None-Match: <etag>` | 304 | 304 |
| `If-None-Match: "bogus"` | 200 | 200 |
| `If-None-Match: *` | 304 | 304 |
| `If-None-Match: W/<etag>` | 304 | 304 |
| `If-None-Match: "aaa", <etag>, "bbb"` | 304 | 304 |
| `If-Modified-Since: <last-modified>` | 304 | 304 |
| `If-Modified-Since: <old date>` | 200 | 200 |
| `If-Match: <etag>` | 200 | 200 |
| `If-Match: "bogus"` | 412 | 412 |
| `If-Unmodified-Since: <old date>` | 412 | 412 |
| `If-Unmodified-Since: <future date>` | 200 | 200 |

The debug log confirms this module made the decisions rather than the built-in one — it was entered 14 times with `status:200`, and its own comparisons appear for every case:

```
rust not_modified: entry status:200 main:true disabled:0
rust not_modified: im:""6a93eb2b-1b"" etag:"6a93eb2b-1b"
rust not_modified: ims:1788078891 lm:1788078891
rust not_modified: iums:631152000 lm:1788078891
```

## A statically linked filter cannot run first

Worth knowing before building any filter into nginx with `--add-module`.

`auto/module` decides where a filter goes:

```sh
if [ -z "$ngx_module_order" -a \
     \( "$ngx_module_type" = "HTTP_FILTER" -o "$ngx_module_type" = "HTTP_AUX_FILTER" \) ]
then
    eval ${ngx_module}_ORDER="$ngx_module_name ngx_http_copy_filter_module"
else
    eval ${ngx_module}_ORDER="$ngx_module_order"
fi
```

Two things follow. The default places the module just before `ngx_http_copy_filter_module`, which in `ngx_modules[]` lands it *ahead of* nginx's own `not_modified` filter — and since filters prepend themselves, being earlier in the array means running **later** in the chain. And `_ORDER` is only ever set inside the dynamic-module branch, so setting `ngx_module_order` in a `config` file changes nothing for a static build.

Built in statically, this module is therefore always entered with the response already converted:

```
rust not_modified: entry status:304 main:true disabled:0
```

It short-circuits correctly, which does show the installation works — but its own logic never decides anything. Loading it dynamically puts it at the head of the chain, because `load_module` runs its `postconfiguration` after every static module's.

## Differences from the C original

Three, all deliberate:

- **Debug messages are prefixed `rust not_modified:`.** The C module logs `http ims:...`; identical text would make it impossible to tell which filter produced a line when both are loaded.
- **There is an extra debug line at filter entry** reporting the status, whether this is the main request, and `disable_not_modified`. This is what made the ordering problem above visible.
- **`etag_list_contains` is a separate function** rather than inline in `test_if_match`, so the list-walking logic can be unit tested without a request.

## Layout

| path | contents |
| --- | --- |
| `src/lib.rs` | module definition, filter installation, the filter and its three helpers |
| `src/etag.rs` | entity-tag list matching — no nginx types |
| `tests/etag_list.rs` | tests for the above |
| `config`, `config.make`, `auto/rust` | static build support for `--add-module` |
