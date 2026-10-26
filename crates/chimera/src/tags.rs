//! Color tags: the `user.chimera.tags` xattr ("red,blue"), which the index also reads for `tag:`.

use chimera_index::{TAG_XATTR, TAGS, tag_bits};
use gtk::gio;
use gtk::prelude::*;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

pub fn of_info(info: &gio::FileInfo) -> u8 {
    info.attribute_string("xattr::chimera.tags").map_or(0, |s| tag_bits(&s))
}

pub fn names(bits: u8) -> impl Iterator<Item = &'static str> {
    TAGS.iter().enumerate().filter(move |(i, _)| bits & (1 << i) != 0).map(|(_, n)| *n)
}

/// Local files only (xattrs don't travel over GVfs).
pub fn write(f: &gio::File, bits: u8) -> std::io::Result<()> {
    let p = f.path().ok_or_else(|| std::io::Error::other("tags need a local file"))?;
    let c = CString::new(p.as_os_str().as_bytes())?;
    let name = CString::new(TAG_XATTR).unwrap();
    let r = if bits == 0 {
        let r = unsafe { libc::lremovexattr(c.as_ptr(), name.as_ptr()) };
        if r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENODATA) {
            0
        } else {
            r
        }
    } else {
        let v = names(bits).collect::<Vec<_>>().join(",");
        unsafe { libc::lsetxattr(c.as_ptr(), name.as_ptr(), v.as_ptr().cast(), v.len(), 0) }
    };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

pub fn read(f: &gio::File) -> u8 {
    f.path().map_or(0, |p| chimera_index::scan::read_tags(&p))
}

/// Small colored dots for a name cell.
pub fn fill_dots(b: &gtk::Box, bits: u8) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
    for n in names(bits) {
        b.append(
            &gtk::Box::builder()
                .css_classes(["tag-dot", &format!("tag-{n}")])
                .valign(gtk::Align::Center)
                .build(),
        );
    }
}
