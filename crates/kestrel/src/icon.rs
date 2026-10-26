//! The app icon's folder takes the colour of the icon theme's own folder (Yaru accents, Papirus,
//! Adwaita…). The packaged icon stays yellow; a tinted copy goes in ~/.local/share/icons/hicolor,
//! which icon lookups search before /usr/share, and is redone whenever the icon theme changes.

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib};

/// From packaging/icon/split.py: the folder as grey shading (128 = the front face) plus alpha,
/// and everything else (the bird, the shadow) untouched.
const FOLDER: &[u8] = include_bytes!("../../../packaging/icon/folder.png");
const BIRD: &[u8] = include_bytes!("../../../packaging/icon/bird.png");

pub fn follow_theme() {
    // after the first window is up: rendering takes a few tens of ms
    glib::idle_add_local_once(apply);
    if let Some(s) = gtk::Settings::default() {
        // the new theme's folder icon is only found once the theme has loaded
        s.connect_gtk_icon_theme_name_notify(|_| {
            glib::idle_add_local_once(apply);
        });
    }
}

fn apply() {
    let dir = glib::user_data_dir().join("icons/hicolor");
    let out = dir.join("scalable/apps/dev.kestrel.Kestrel.svg");
    let svg = theme_folder_colour().and_then(|c| render(c).ok());
    let Some(svg) = svg else {
        // grey or unreadable folder icon: the packaged yellow one
        if std::fs::remove_file(&out).is_ok() {
            touch(&dir);
        }
        return;
    };
    if std::fs::read(&out).ok().as_deref() == Some(svg.as_bytes()) {
        return;
    }
    if std::fs::create_dir_all(out.parent().unwrap()).is_ok() && std::fs::write(&out, svg).is_ok() {
        touch(&dir);
    }
}

/// Icon lookups re-scan a theme folder when its mtime changes.
fn touch(dir: &std::path::Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// The front-face colour of the theme's "folder" icon: its most common saturated colour.
fn theme_folder_colour() -> Option<[u8; 3]> {
    let theme = gtk::IconTheme::for_display(&gdk::Display::default()?);
    let icon = theme.lookup_icon("folder", &[], 128, 1, gtk::TextDirection::Ltr, gtk::IconLookupFlags::empty());
    let pb = gdk_pixbuf::Pixbuf::from_file_at_size(icon.file()?.path()?, 128, 128).ok()?;
    if pb.n_channels() != 4 {
        return None;
    }
    let (bytes, stride) = (pb.read_pixel_bytes(), pb.rowstride() as usize);
    let rows = (0..pb.height() as usize).map(|y| &bytes[y * stride..y * stride + pb.width() as usize * 4]);
    dominant(rows.flat_map(|r| r.as_chunks::<4>().0.iter().map(|px| px.as_slice())))
}

fn dominant<'a>(px: impl Iterator<Item = &'a [u8]>) -> Option<[u8; 3]> {
    let mut bins = std::collections::HashMap::<[u8; 3], (u32, [u32; 3])>::new();
    for p in px {
        let (max, min) = (p[0].max(p[1]).max(p[2]) as i32, p[0].min(p[1]).min(p[2]) as i32);
        // opaque and clearly coloured (skips outlines, highlights, emblems)
        if p[3] < 250 || max - min < 60 {
            continue;
        }
        let e = bins.entry([p[0] >> 3, p[1] >> 3, p[2] >> 3]).or_default();
        e.0 += 1;
        (0..3).for_each(|i| e.1[i] += p[i] as u32);
    }
    let (n, sum) = bins.into_values().max_by_key(|(n, _)| *n)?;
    Some(sum.map(|s| (s / n) as u8))
}

/// The tinted icon as an SVG wrapping a 512 px PNG (the same shape as the packaged one).
fn render(c: [u8; 3]) -> Result<String, glib::Error> {
    let load = |b: &'static [u8]| gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(b)).map_err(|e| glib::Error::new(glib::FileError::Inval, &e.to_string()));
    let (folder, bird) = (load(FOLDER)?, load(BIRD)?);
    let (w, h) = (bird.width() as usize, bird.height() as usize);
    let (fb, fs, fc) = (folder.read_pixel_bytes(), folder.rowstride() as usize, folder.n_channels() as usize);
    let (bb, bs) = (bird.read_pixel_bytes(), bird.rowstride() as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let f = &fb[y * fs + x * fc..];
            let b = &bb[y * bs + x * 4..];
            // folder: shade the theme colour (128 = as is); then the bird over it
            let fa = if fc == 4 { f[3] } else { 255 } as f32 / 255.0;
            let shade = |i: usize| (c[i] as f32 * f[0] as f32 / 128.0).min(255.0);
            let ba = b[3] as f32 / 255.0;
            let a = ba + fa * (1.0 - ba);
            let o = &mut out[(y * w + x) * 4..][..4];
            if a > 0.0 {
                for i in 0..3 {
                    o[i] = ((b[i] as f32 * ba + shade(i) * fa * (1.0 - ba)) / a).round() as u8;
                }
            }
            o[3] = (a * 255.0).round() as u8;
        }
    }
    let pb = gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(out),
        gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        w as i32,
        h as i32,
        w as i32 * 4,
    );
    let png = pb.save_to_bufferv("png", &[])?;
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"128\" height=\"128\" viewBox=\"0 0 128 128\">\n  <!-- Kestrel icon tinted to the icon theme's folder colour (written by Kestrel) -->\n  <image width=\"128\" height=\"128\" xlink:href=\"data:image/png;base64,{}\"/>\n</svg>\n",
        glib::base64_encode(&png)
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dominant_skips_grey_and_transparent() {
        let px: Vec<[u8; 4]> = [[104, 176, 168, 255]; 5]
            .into_iter()
            .chain([[48, 128, 128, 255]; 2]) // tab shade
            .chain([[200, 200, 200, 255]; 9]) // grey outline: skipped
            .chain([[255, 0, 0, 100]; 9]) // translucent: skipped
            .collect();
        assert_eq!(super::dominant(px.iter().map(|p| &p[..])), Some([104, 176, 168]));
        assert_eq!(super::dominant([[90u8, 90, 90, 255]].iter().map(|p| &p[..])), None);
    }
}
