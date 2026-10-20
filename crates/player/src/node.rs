// SPDX-License-Identifier: GPL-3.0-or-later
//! Building `mpv_node` trees to pass to mpv, and reading the ones mpv hands
//! back.

use std::ffi::{CString, c_char};

use crate::ffi::{self, mpv_node, mpv_node_list, mpv_node_u};

/// A value to pass to mpv as an `mpv_node`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeIn<'a> {
    Str(&'a str),
    Map(Vec<(&'a str, NodeIn<'a>)>),
}

/// An `mpv_node` tree whose memory is owned by Rust. mpv only reads it, and
/// copies whatever it keeps, so it must stay alive only for the call.
pub(crate) struct OwnedNode {
    root: mpv_node,
    // Heap allocations the raw pointers in `root` point into. Moving these
    // containers does not move their heap buffers.
    _strings: Vec<CString>,
    _values: Vec<Box<[mpv_node]>>,
    _keys: Vec<Box<[*mut c_char]>>,
    // Boxed on purpose: `root`/parent nodes point at these lists, so they
    // must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    _lists: Vec<Box<mpv_node_list>>,
}

impl OwnedNode {
    /// Builds the C representation. Fails if a string contains NUL.
    pub(crate) fn new(value: &NodeIn<'_>) -> Result<Self, std::ffi::NulError> {
        let mut b = Self {
            root: mpv_node {
                u: mpv_node_u { int64: 0 },
                format: ffi::MPV_FORMAT_NONE,
            },
            _strings: Vec::new(),
            _values: Vec::new(),
            _keys: Vec::new(),
            _lists: Vec::new(),
        };
        b.root = b.build(value)?;
        Ok(b)
    }

    fn cstr(&mut self, s: &str) -> Result<*mut c_char, std::ffi::NulError> {
        let c = CString::new(s)?;
        let p = c.as_ptr().cast_mut();
        self._strings.push(c);
        Ok(p)
    }

    fn build(&mut self, v: &NodeIn<'_>) -> Result<mpv_node, std::ffi::NulError> {
        Ok(match v {
            NodeIn::Str(s) => mpv_node {
                u: mpv_node_u {
                    string: self.cstr(s)?,
                },
                format: ffi::MPV_FORMAT_STRING,
            },
            NodeIn::Map(entries) => {
                let mut values = Vec::with_capacity(entries.len());
                let mut keys = Vec::with_capacity(entries.len());
                for (k, child) in entries {
                    keys.push(self.cstr(k)?);
                    values.push(self.build(child)?);
                }
                let mut values = values.into_boxed_slice();
                let mut keys = keys.into_boxed_slice();
                let mut list = Box::new(mpv_node_list {
                    // Maps we build have a handful of entries.
                    num: i32::try_from(values.len()).unwrap_or(i32::MAX),
                    values: values.as_mut_ptr(),
                    keys: keys.as_mut_ptr(),
                });
                let list_ptr: *mut mpv_node_list = &mut *list;
                self._values.push(values);
                self._keys.push(keys);
                self._lists.push(list);
                mpv_node {
                    u: mpv_node_u { list: list_ptr },
                    format: ffi::MPV_FORMAT_NODE_MAP,
                }
            }
        })
    }

    /// Pointer to the root node, valid while `self` lives.
    pub(crate) fn as_mut_ptr(&mut self) -> *mut mpv_node {
        &mut self.root
    }
}

/// A value read back from mpv (e.g. the `track-list` property).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeOut {
    None,
    Str(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    Array(Vec<NodeOut>),
    Map(Vec<(String, NodeOut)>),
}

/// Media can contain arbitrarily many tracks/tags; bound what we copy.
const MAX_DEPTH: usize = 16;
const MAX_ITEMS: usize = 4096;

impl NodeOut {
    /// Deep-copies an mpv-owned node.
    ///
    /// # Safety
    /// `node` must point to a valid `mpv_node` (as found in a
    /// `MPV_EVENT_PROPERTY_CHANGE` with `MPV_FORMAT_NODE`) that stays valid
    /// for the duration of the call.
    pub(crate) unsafe fn from_raw(node: *const mpv_node) -> Self {
        // SAFETY: forwarded from the caller.
        unsafe { Self::from_raw_depth(node, 0) }
    }

    unsafe fn from_raw_depth(node: *const mpv_node, depth: usize) -> Self {
        if node.is_null() || depth > MAX_DEPTH {
            return Self::None;
        }
        // SAFETY: caller guarantees `node` is valid; we only read the union
        // member that `format` says is active.
        let n = unsafe { &*node };
        match n.format {
            ffi::MPV_FORMAT_STRING => {
                // SAFETY: format says `string` is active; mpv strings are
                // NUL-terminated (or NULL).
                let s = unsafe { ffi::opt_string(n.u.string) };
                s.map_or(Self::None, Self::Str)
            }
            // SAFETY: format says `flag` is active.
            ffi::MPV_FORMAT_FLAG => Self::Flag(unsafe { n.u.flag } != 0),
            // SAFETY: format says `int64` is active.
            ffi::MPV_FORMAT_INT64 => Self::Int(unsafe { n.u.int64 }),
            // SAFETY: format says `double_` is active.
            ffi::MPV_FORMAT_DOUBLE => Self::Double(unsafe { n.u.double_ }),
            ffi::MPV_FORMAT_NODE_ARRAY | ffi::MPV_FORMAT_NODE_MAP => {
                // SAFETY: format says `list` is active.
                let list = unsafe { n.u.list };
                if list.is_null() {
                    return Self::None;
                }
                // SAFETY: non-null list pointer owned by mpv, valid per caller.
                let list = unsafe { &*list };
                let count = usize::try_from(list.num).unwrap_or(0).min(MAX_ITEMS);
                let is_map = n.format == ffi::MPV_FORMAT_NODE_MAP;
                let mut arr = Vec::new();
                let mut map = Vec::new();
                for i in 0..count {
                    // SAFETY: i < num, so values[i] is in bounds.
                    let child = unsafe { Self::from_raw_depth(list.values.add(i), depth + 1) };
                    if is_map {
                        if list.keys.is_null() {
                            break;
                        }
                        // SAFETY: maps have `num` keys; each is a C string.
                        let key = unsafe { ffi::opt_string(*list.keys.add(i)) }.unwrap_or_default();
                        map.push((key, child));
                    } else {
                        arr.push(child);
                    }
                }
                if is_map {
                    Self::Map(map)
                } else {
                    Self::Array(arr)
                }
            }
            _ => Self::None,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&NodeOut> {
        match self {
            Self::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            // Truncation is fine: only used for small integral values.
            #[allow(clippy::cast_possible_truncation)]
            Self::Double(d) if d.is_finite() => Some(*d as i64),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Flag(b) => Some(*b),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_map_round_trips_through_reader() {
        let v = NodeIn::Map(vec![
            ("name", NodeIn::Str("loadfile")),
            ("url", NodeIn::Str("http://example.com/x.ts")),
            ("options", NodeIn::Map(vec![("start", NodeIn::Str("12.5"))])),
        ]);
        let Ok(mut owned) = OwnedNode::new(&v) else {
            panic!("build failed");
        };
        // SAFETY: `owned` is alive and well-formed.
        let back = unsafe { NodeOut::from_raw(owned.as_mut_ptr()) };
        assert_eq!(back.get("name").and_then(NodeOut::as_str), Some("loadfile"));
        assert_eq!(
            back.get("options")
                .and_then(|o| o.get("start"))
                .and_then(NodeOut::as_str),
            Some("12.5")
        );
        assert_eq!(back.get("missing"), None);
    }

    #[test]
    fn nul_in_string_is_rejected() {
        assert!(OwnedNode::new(&NodeIn::Str("a\0b")).is_err());
        assert!(OwnedNode::new(&NodeIn::Map(vec![("k\0", NodeIn::Str("v"))])).is_err());
    }

    #[test]
    fn reader_handles_null() {
        // SAFETY: NULL is explicitly handled.
        let v = unsafe { NodeOut::from_raw(std::ptr::null()) };
        assert_eq!(v, NodeOut::None);
    }
}
