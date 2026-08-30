// Entity-tag list matching, split out from the filter itself.
//
// Nothing here touches nginx, so it can be compiled and tested on its own.
// `tests/etag_list.rs` includes this file directly for that reason, which is
// why the module documentation is a plain comment rather than `//!`.

/// Walks a comma-separated entity-tag list looking for `etag`.
///
/// Split out from [`test_if_match`] so that it can be unit tested without a
/// request: it is pure byte slicing with no nginx types involved.
pub(crate) fn etag_list_contains(list: &[u8], etag: &[u8], weak: bool) -> bool {
    let mut rest = list;

    while !rest.is_empty() {
        if weak && rest.len() > 2 && rest.starts_with(b"W/") {
            rest = &rest[2..];
        }

        if etag.len() > rest.len() {
            return false;
        }

        if rest.starts_with(etag) {
            let after = trim_start_spaces(&rest[etag.len()..]);
            if after.is_empty() || after[0] == b',' {
                return true;
            }
        }

        // Skip to just past the next comma, then over any padding.
        let comma = rest.iter().position(|&c| c == b',');
        rest = match comma {
            Some(i) => trim_start_list_padding(&rest[i..]),
            None => &[],
        };
    }

    false
}

fn trim_start_spaces(mut s: &[u8]) -> &[u8] {
    while let [first, tail @ ..] = s {
        if *first == b' ' || *first == b'\t' {
            s = tail;
        } else {
            break;
        }
    }
    s
}

fn trim_start_list_padding(mut s: &[u8]) -> &[u8] {
    while let [first, tail @ ..] = s {
        if *first == b' ' || *first == b'\t' || *first == b',' {
            s = tail;
        } else {
            break;
        }
    }
    s
}
