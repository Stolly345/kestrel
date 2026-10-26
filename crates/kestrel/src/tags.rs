//! Color tags: the `user.chimera.tags` xattr ("red,blue"), which the index also reads for `tag:`.

use kestrel_index::{TAG_XATTR, TAGS, tag_bits};
use gtk::gio;
use gtk::glib::prelude::*;
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
    f.path().map_or(0, |p| kestrel_index::scan::read_tags(&p))
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

fn title(t: &str) -> String {
    format!("{}{}", t[..1].to_uppercase(), &t[1..])
}

/// A round colour button for tag `t` (the dot is a child, so the theme's hover ring shows around it).
pub fn dot_button(t: &str) -> gtk::Button {
    let dot = gtk::Box::builder()
        .css_classes(["tag-dot", "tag-big", &format!("tag-{t}")])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let b = gtk::Button::builder()
        .child(&dot)
        .css_classes(["flat", "circular", "tag-pick"])
        .tooltip_text(title(t))
        .focus_on_click(false)
        .build();
    b.update_property(&[gtk::accessible::Property::Label(&title(t))]);
    b
}

/// Toggle tag `t` on `files` ("none" clears): if every file has it, remove it; else add it to all.
pub fn toggle(files: &[gio::File], t: &str) -> std::io::Result<()> {
    let bit = tag_bits(t);
    let all = files.iter().all(|f| read(f) & bit != 0);
    for f in files {
        let cur = read(f);
        write(f, if t == "none" { 0 } else if all { cur & !bit } else { cur | bit })?;
    }
    Ok(())
}

/// Finder-style tag picker: one dot per colour (checked where `set_picker` says) plus Clear.
/// Clicking calls `pick` with the colour, or "none" for Clear.
pub fn picker(pick: impl Fn(&str) + Clone + 'static) -> gtk::Box {
    let row = gtk::Box::builder().spacing(2).halign(gtk::Align::Center).css_classes(["tag-picker"]).build();
    for t in TAGS {
        let b = dot_button(t);
        b.set_tooltip_text(Some(&format!("Tag {}", title(t))));
        let p = pick.clone();
        b.connect_clicked(move |_| p(t));
        row.append(&b);
    }
    let clear = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .css_classes(["flat", "circular", "tag-pick"])
        .tooltip_text("Clear tags")
        .build();
    clear.connect_clicked(move |_| pick("none"));
    row.append(&clear);
    crate::label_icon_buttons(row.upcast_ref());
    row
}

/// Check the dots for `bits` (the tags shared by everything selected).
pub fn set_picker(row: &gtk::Box, bits: u8) {
    let mut c = row.first_child();
    for i in 0..TAGS.len() {
        let Some(btn) = c else { break };
        c = btn.next_sibling();
        let Some(dot) = btn.first_child().and_downcast::<gtk::Box>() else { continue };
        let on = bits & (1 << i) != 0;
        if dot.first_child().is_some() != on {
            if on {
                dot.append(&gtk::Image::builder().icon_name("object-select-symbolic").pixel_size(12).hexpand(true).build());
            } else if let Some(img) = dot.first_child() {
                dot.remove(&img);
            }
        }
        btn.update_state(&[gtk::accessible::State::Pressed(if on { gtk::AccessibleTristate::True } else { gtk::AccessibleTristate::False })]);
    }
}
