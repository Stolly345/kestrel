//! Preview pane: picture, video/audio player, text, rendered Markdown, PDF/Office pages,
//! document thumbnail, or folder totals.

use crate::browser::{fmt_date, info_file, set_icon};
use crate::thumbs;
use adw::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};

pub struct Preview {
    pub root: gtk::Box,
    body: gtk::Box,
    /// Bumped on every show(); late async results for an older selection are dropped.
    serial: std::rc::Rc<std::cell::Cell<u64>>,
}

fn row(grid: &gtk::Grid, n: i32, k: &str, v: &str) {
    if v.is_empty() {
        return;
    }
    grid.attach(
        &gtk::Label::builder().label(k).xalign(1.0).yalign(0.0).css_classes(["dim-label"]).build(),
        0,
        n,
        1,
        1,
    );
    grid.attach(
        &gtk::Label::builder()
            .label(v)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .hexpand(true)
            .build(),
        1,
        n,
        1,
        1,
    );
}

pub fn perms(mode: u32) -> String {
    let bits = ["r", "w", "x"];
    (0..9).map(|i| if mode & (1 << (8 - i)) != 0 { bits[i % 3] } else { "-" }).collect::<String>() + &format!("  ({:o})", mode & 0o7777)
}

const TEXT_LIMIT: usize = 256 * 1024;

/// Word, Excel, PowerPoint, OpenDocument and RTF: LibreOffice turns them into a PDF first.
/// "<p align=center>Hi <b>there</b></p>" -> "Hi there"
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    if out.trim().is_empty() { String::new() } else { out }
}

/// A Kiwix archive: its icon, title, description and what's in it.
fn zim_card(path: std::path::PathBuf, info: &gio::FileInfo) -> gtk::Box {
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(6).build();
    let icon = gtk::Image::builder().pixel_size(96).build();
    set_icon(&icon, info);
    icon.set_pixel_size(96);
    let title = gtk::Label::builder()
        .label(info.display_name().as_str())
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-3"])
        .build();
    let desc = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).visible(false).build();
    let meta = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label", "caption"])
        .label("Reading…")
        .build();
    for w in [icon.upcast_ref::<gtk::Widget>(), title.upcast_ref(), desc.upcast_ref(), meta.upcast_ref()] {
        card.append(w);
    }
    glib::spawn_future_local(async move {
        let r = gio::spawn_blocking(move || crate::zim::info(&path)).await;
        let z = match r {
            Ok(Ok(z)) => z,
            Ok(Err(e)) => return meta.set_label(&format!("Can't read this archive: {}", crate::browser::human_io(&e))),
            Err(_) => return,
        };
        if let Some(t) = z.title {
            title.set_label(&t);
        }
        if let Some(d) = z.description {
            desc.set_label(&d);
            desc.set_visible(true);
        }
        if let Some(t) = z.icon.and_then(|png| gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(png)).ok()) {
            icon.set_paintable(Some(&t));
        }
        let mut bits = vec![match z.articles {
            Some(n) => format!("{} articles", thousands(n)),
            None => format!("{} entries", thousands(z.entries as u64)),
        }];
        bits.extend(z.language);
        bits.extend(z.date);
        bits.extend(z.creator.or(z.publisher));
        meta.set_label(&format!("Kiwix offline archive\n{}", bits.join(" \u{b7} ")));
    });
    card
}

/// 6863660 -> "6,863,660"
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a music file's tags say, read off the GTK thread.
#[derive(Default)]
struct Song {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year: Option<u16>,
    secs: u64,
    kbps: Option<u32>,
    cover: Option<gtk::gdk::Texture>,
}

fn read_song(path: &std::path::Path) -> Song {
    use lofty::prelude::*;
    let Ok(f) = lofty::read_from_path(path) else { return Song::default() };
    let p = f.properties();
    let mut s = Song {
        secs: p.duration().as_secs(),
        kbps: p.audio_bitrate(),
        ..Default::default()
    };
    if let Some(t) = f.primary_tag().or_else(|| f.first_tag()) {
        s.title = t.title().map(|x| x.into_owned());
        s.artist = t.artist().map(|x| x.into_owned());
        s.album = t.album().map(|x| x.into_owned());
        s.year = t.date().map(|d| d.year);
        s.cover = t
            .pictures()
            .iter()
            .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
            .or(t.pictures().first())
            .and_then(|p| gtk::gdk::Texture::from_bytes(&glib::Bytes::from(p.data())).ok());
    }
    s
}

/// A music file: cover art, title / artist / album, and a small player: previous / play / next,
/// a seek bar and repeat, through the other songs in the same folder.
///
/// Played by Kestrel's own GStreamer pipeline (playbin, audio only) rather than GtkMediaFile:
/// GTK always attaches a video sink, and GStreamer 1.28's decodebin3 aborts the whole process on
/// some MP3s when one is attached ("mq_slot_handle_stream_start: assertion failed: (collection)").
/// Nothing plays until you press Play, so selecting songs costs nothing.
fn music(file: &gio::File, info: &gio::FileInfo) -> gtk::Box {
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(6).build();
    let w = std::rc::Rc::new(SongWidgets {
        art: gtk::Image::builder()
            .pixel_size(160)
            .icon_name("audio-x-generic")
            .css_classes(["music-art"])
            .build(),
        pic: gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .height_request(180)
            .visible(false)
            .css_classes(["music-art"])
            .build(),
        title: gtk::Label::builder()
            .label(info.display_name().as_str())
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-3"])
            .build(),
        by: gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).visible(false).build(),
        meta: gtk::Label::builder().css_classes(["dim-label", "caption"]).visible(false).build(),
        error: gtk::Label::builder().wrap(true).css_classes(["error", "caption"]).visible(false).build(),
    });
    for x in [
        w.art.upcast_ref::<gtk::Widget>(),
        w.pic.upcast_ref(),
        w.title.upcast_ref(),
        w.by.upcast_ref(),
        w.meta.upcast_ref(),
    ] {
        card.append(x);
    }
    let seek = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .hexpand(true)
        .sensitive(false)
        .build();
    seek.set_range(0.0, 1.0);
    let time = gtk::Label::builder().label("0:00").css_classes(["dim-label", "caption", "numeric"]).build();
    let row = gtk::Box::builder().spacing(6).build();
    row.append(&seek);
    row.append(&time);
    let button = |icon: &str, tip: &str| {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build()
    };
    let prev = button("media-skip-backward-symbolic", "Previous song in this folder");
    let next = button("media-skip-forward-symbolic", "Next song in this folder");
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .width_request(56)
        .height_request(56)
        .css_classes(["circular", "suggested-action", "music-play"])
        .build();
    let repeat = button("media-playlist-consecutive-symbolic", "Repeat: off");
    let buttons = gtk::CenterBox::builder().build();
    let mid = gtk::Box::builder().spacing(12).build();
    mid.append(&prev);
    mid.append(&play);
    mid.append(&next);
    buttons.set_center_widget(Some(&mid));
    buttons.set_end_widget(Some(&repeat));
    card.append(&buttons);
    card.append(&row);
    card.append(&w.error);
    show_song(&w, file.path());

    let player = std::rc::Rc::new(Player {
        pipeline: RefCell::new(None),
        songs: RefCell::new(file.path().into_iter().collect()),
        at: Cell::new(0),
        repeat: Cell::new(Repeat::Off),
        w,
        seek: seek.clone(),
        time,
        play: play.clone(),
        dragging: Cell::new(false),
    });
    // the folder's other songs, for previous / next
    if let Some(path) = file.path() {
        let p = player.clone();
        glib::spawn_future_local(async move {
            let here = path.clone();
            if let Ok(list) = gio::spawn_blocking(move || songs_beside(&here)).await
                && let Some(at) = list.iter().position(|x| *x == path)
            {
                *p.songs.borrow_mut() = list;
                p.at.set(at);
            }
        });
    }
    let p = player.clone();
    play.connect_clicked(move |_| p.toggle());
    let p = player.clone();
    prev.connect_clicked(move |_| p.step(-1, true));
    let p = player.clone();
    next.connect_clicked(move |_| p.step(1, true));
    let p = player.clone();
    repeat.connect_clicked(move |b| {
        let r = p.repeat.get().next();
        p.repeat.set(r);
        b.set_icon_name(r.icon());
        b.set_tooltip_text(Some(r.label()));
    });
    let p = player.clone();
    seek.connect_change_value(move |_, _, v| {
        p.dragging.set(true); // the next tick leaves the knob where the hand put it
        if let Some(pl) = p.pipeline.borrow().as_ref() {
            let _ = pl.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::from_seconds_f64(v.max(0.0)));
        }
        glib::Propagation::Proceed
    });
    // position, and stop when the preview moves on to another file
    let weak = std::rc::Rc::downgrade(&player);
    glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
        p.tick();
        glib::ControlFlow::Continue
    });
    let p = player.clone();
    card.connect_unmap(move |_| p.stop());
    // the card owns the player: dropped with it (the timer holds only a weak ref)
    unsafe { card.set_data("kestrel-player", player) };
    card
}

#[derive(Clone, Copy, PartialEq)]
enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Repeat::Off => "media-playlist-consecutive-symbolic",
            Repeat::All => "media-playlist-repeat-symbolic",
            Repeat::One => "media-playlist-repeat-song-symbolic",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Repeat::Off => "Repeat: off",
            Repeat::All => "Repeat: this folder",
            Repeat::One => "Repeat: this song",
        }
    }
}

struct SongWidgets {
    art: gtk::Image,
    pic: gtk::Picture,
    title: gtk::Label,
    by: gtk::Label,
    meta: gtk::Label,
    error: gtk::Label,
}

struct Player {
    pipeline: RefCell<Option<gst::Element>>,
    songs: RefCell<Vec<std::path::PathBuf>>,
    at: Cell<usize>,
    repeat: Cell<Repeat>,
    w: std::rc::Rc<SongWidgets>,
    seek: gtk::Scale,
    time: gtk::Label,
    play: gtk::Button,
    dragging: Cell<bool>,
}

impl Player {
    fn current(&self) -> Option<std::path::PathBuf> {
        self.songs.borrow().get(self.at.get()).cloned()
    }

    fn toggle(self: &std::rc::Rc<Self>) {
        let state = self.pipeline.borrow().as_ref().map(|p| p.current_state());
        match state {
            Some(gst::State::Playing) => self.set(gst::State::Paused),
            Some(_) => self.set(gst::State::Playing),
            None => self.start(),
        }
    }

    fn set(&self, s: gst::State) {
        if let Some(p) = self.pipeline.borrow().as_ref() {
            let _ = p.set_state(s);
        }
        let playing = s == gst::State::Playing;
        self.play.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        self.play.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
    }

    fn stop(&self) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.play.set_icon_name("media-playback-start-symbolic");
        self.seek.set_sensitive(false);
    }

    /// Build the pipeline for the current song and play it.
    fn start(self: &std::rc::Rc<Self>) {
        self.stop();
        self.w.error.set_visible(false);
        let Some(path) = self.current() else { return };
        if gst::init().is_err() {
            return self.fail("GStreamer isn't available");
        }
        let Ok(pb) = gst::ElementFactory::make("playbin")
            .property("uri", gio::File::for_path(&path).uri().as_str())
            .build()
        else {
            return self.fail("GStreamer's playbin is missing (gstreamer1.0-plugins-base)");
        };
        pb.set_property_from_str("flags", "audio+soft-volume"); // audio only: see music()
        let bus = pb.bus().unwrap();
        let weak = std::rc::Rc::downgrade(self);
        let guard = bus.add_watch_local(move |_, msg| {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            match msg.view() {
                gst::MessageView::Eos(_) => p.ended(),
                gst::MessageView::Error(e) => {
                    let why = if e.error().matches(gst::CoreError::MissingPlugin) || e.error().matches(gst::StreamError::CodecNotFound) {
                        "no codec for its format is installed (sudo apt install gstreamer1.0-libav)".to_string()
                    } else {
                        e.error().message().to_string()
                    };
                    p.stop();
                    p.fail(&why);
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
        // the watch lives as long as the pipeline; dropping the guard would remove it
        if let Ok(g) = guard {
            unsafe { pb.set_data("kestrel-bus-watch", g) };
        }
        *self.pipeline.borrow_mut() = Some(pb);
        self.set(gst::State::Playing);
        self.seek.set_sensitive(true);
    }

    fn fail(&self, why: &str) {
        self.w.error.set_label(&format!("Can't play this: {why}"));
        self.w.error.set_visible(true);
    }

    /// Previous / next song in the folder; `wrap` = go round from the last to the first.
    fn step(self: &std::rc::Rc<Self>, by: isize, wrap: bool) {
        let n = self.songs.borrow().len() as isize;
        if n == 0 {
            return;
        }
        let to = self.at.get() as isize + by;
        let to = if wrap {
            to.rem_euclid(n)
        } else if (0..n).contains(&to) {
            to
        } else {
            return self.stop();
        };
        let playing = self.pipeline.borrow().is_some();
        self.at.set(to as usize);
        show_song(&self.w, self.current());
        self.seek.set_value(0.0);
        self.time.set_label("0:00");
        if playing { self.start() } else { self.stop() }
    }

    fn ended(self: &std::rc::Rc<Self>) {
        match self.repeat.get() {
            Repeat::One => self.start(),
            Repeat::All => self.step(1, true),
            Repeat::Off => {
                let last = self.at.get() + 1 >= self.songs.borrow().len();
                if last { self.stop() } else { self.step(1, false) }
            }
        }
    }

    fn tick(&self) {
        let pl = self.pipeline.borrow();
        let Some(pl) = pl.as_ref() else { return };
        let (Some(pos), dur) = (pl.query_position::<gst::ClockTime>(), pl.query_duration::<gst::ClockTime>()) else {
            return;
        };
        let mmss = |t: gst::ClockTime| format!("{}:{:02}", t.seconds() / 60, t.seconds() % 60);
        if let Some(d) = dur {
            self.seek.set_range(0.0, d.seconds_f64().max(1.0));
            self.time.set_label(&format!("{} / {}", mmss(pos), mmss(d)));
        } else {
            self.time.set_label(&mmss(pos));
        }
        if !self.dragging.replace(false) {
            self.seek.set_value(pos.seconds_f64());
        }
    }
}

/// Fill the card with a song's tags and cover (read off the GTK thread).
fn show_song(w: &std::rc::Rc<SongWidgets>, path: Option<std::path::PathBuf>) {
    let Some(path) = path else { return };
    w.title.set_label(&path.file_name().unwrap_or_default().to_string_lossy());
    w.by.set_visible(false);
    w.meta.set_visible(false);
    w.pic.set_visible(false);
    w.art.set_visible(true);
    let w = w.clone();
    glib::spawn_future_local(async move {
        let Ok(s) = gio::spawn_blocking(move || read_song(&path)).await else { return };
        if let Some(t) = &s.title {
            w.title.set_label(t);
        }
        let who: Vec<&str> = [s.artist.as_deref(), s.album.as_deref()].into_iter().flatten().collect();
        w.by.set_label(&who.join(" \u{2014} "));
        w.by.set_visible(!who.is_empty());
        let mut bits = Vec::new();
        if let Some(y) = s.year {
            bits.push(y.to_string());
        }
        if s.secs > 0 {
            bits.push(format!("{}:{:02}", s.secs / 60, s.secs % 60));
        }
        if let Some(k) = s.kbps.filter(|k| *k > 0) {
            bits.push(format!("{k} kbps"));
        }
        w.meta.set_label(&bits.join(" \u{b7} "));
        w.meta.set_visible(!bits.is_empty());
        if let Some(t) = s.cover {
            w.pic.set_paintable(Some(&t));
            w.pic.set_visible(true);
            w.art.set_visible(false);
        }
    });
}

/// The audio files in `song`'s folder, in name order (what previous / next walk through).
fn songs_beside(song: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(dir) = song.parent() else { return vec![song.to_path_buf()] };
    let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && gio::content_type_guess(Some(p), None).0.starts_with("audio/"))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v
}

/// A stream that can't play (no codec for it, a broken file) says so under the player instead of
/// sitting there black or silent.
fn say_media_errors(m: &gtk::MediaStream, under: &gtk::Box) {
    let under = under.downgrade();
    m.connect_error_notify(move |m| {
        let (Some(e), Some(b)) = (m.error(), under.upgrade()) else { return };
        let why = if e.matches(gio::IOErrorEnum::NotSupported) || e.message().contains("codec") || e.message().contains("plugin") {
            "Can't play this: no codec for its format is installed (sudo apt install gstreamer1.0-libav)".to_string()
        } else {
            format!("Can't play this: {}", crate::browser::human_gio(&e))
        };
        b.append(&gtk::Label::builder().label(why).wrap(true).css_classes(["error", "caption"]).build());
    });
}

/// GTK's loaders (glycin: sandboxed, most formats), then gdk-pixbuf's (a few glycin lacks, e.g. XBM).
fn load_image(f: &gio::File) -> Option<gtk::gdk::Texture> {
    gtk::gdk::Texture::from_file(f).ok().or_else(|| {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(f.path()?).ok()?;
        #[allow(deprecated)] // for_pixbuf: the only way from a Pixbuf to a Texture in gtk4-rs
        Some(gtk::gdk::Texture::for_pixbuf(&pb))
    })
}

fn is_office(ct: &str) -> bool {
    [
        "application/msword",
        "application/rtf",
        "text/rtf",
        "application/vnd.ms-excel",
        "application/vnd.ms-powerpoint",
        "application/vnd.ms-word",
        "application/vnd.openxmlformats-officedocument.",
        "application/vnd.oasis.opendocument.",
    ]
    .iter()
    .any(|t| ct.starts_with(t))
}

/// A document to page through: its PDF (LibreOffice's conversion, for Office files) and page
/// count. Pages render one at a time as they're viewed, into `dir`
/// (~/.cache/kestrel/preview/<hash of path, size, mtime>). LibreOffice and Poppler parse hostile
/// files, so both run in the archive sandbox (no network, no home, only `dir` writable).
struct Doc {
    pdf: std::path::PathBuf,
    dir: std::path::PathBuf,
    pages: u32,
}

fn cache_root() -> std::path::PathBuf {
    glib::user_cache_dir().join("kestrel/preview")
}

const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 86400);
const CACHE_MAX_BYTES: u64 = 512 << 20;

/// Drop document caches not viewed for 30 days, then the least recently viewed until the
/// whole cache is under 512 MB.
fn prune(root: &std::path::Path, max_age: std::time::Duration, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let now = std::time::SystemTime::now();
    let mut dirs: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let used = e.metadata().ok()?.modified().ok()?;
            // flat: doc.pdf, pages, p<N>.png
            let bytes = std::fs::read_dir(e.path())
                .ok()?
                .flatten()
                .filter_map(|f| f.metadata().ok())
                .map(|m| m.len())
                .sum();
            Some((used, bytes, e.path()))
        })
        .collect();
    dirs.sort(); // oldest first
    let mut total: u64 = dirs.iter().map(|d| d.1).sum();
    for (used, bytes, path) in dirs {
        if now.duration_since(used).unwrap_or_default() < max_age && total <= max_bytes {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            total -= bytes;
        }
    }
}

/// The archive's contents as a summary line and a path per line (folders end in /).
fn archive_text(entries: &[crate::archive::Entry]) -> (String, String) {
    const SHOW: usize = 1000;
    let (dirs, files): (Vec<_>, Vec<_>) = entries.iter().partition(|e| e.dir);
    let bytes: u64 = files.iter().map(|e| e.size).sum();
    let s = |n: usize, w: &str| if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") };
    let summary = format!(
        "{}, {} · {} unpacked",
        s(files.len(), "file"),
        s(dirs.len(), "folder"),
        glib::format_size(bytes)
    );
    let mut list: Vec<String> = entries
        .iter()
        .take(SHOW)
        .map(|e| if e.dir { format!("{}/", e.path) } else { e.path.clone() })
        .collect();
    if entries.len() > SHOW {
        list.push(format!("…and {} more", entries.len() - SHOW));
    }
    (summary, list.join("\n"))
}

fn open_doc(path: &std::path::Path, office: bool, stamp: (i64, i64)) -> Option<Doc> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, stamp).hash(&mut h);
    let dir = cache_root().join(format!("{:016x}", h.finish()));
    std::fs::create_dir_all(&dir).ok()?;
    // mtime = last viewed, which is what prune() goes by
    let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    let pdf = if office {
        let pdf = dir.join("doc.pdf");
        if !pdf.exists() {
            let mut c = crate::archive::sandboxed("soffice", path, &dir);
            c.arg(format!("-env:UserInstallation=file://{}/profile", dir.display()))
                .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
                .arg(&dir)
                .arg(path);
            let _ = c.output();
            let _ = std::fs::remove_dir_all(dir.join("profile"));
            std::fs::rename(dir.join(format!("{}.pdf", path.file_stem()?.to_string_lossy())), &pdf).ok()?;
        }
        pdf
    } else {
        path.to_path_buf()
    };
    let count = dir.join("pages");
    let pages = match std::fs::read_to_string(&count).ok().and_then(|s| s.trim().parse().ok()) {
        Some(n) => n,
        None => {
            let out = crate::archive::sandboxed("pdfinfo", &pdf, &dir).arg(&pdf).output().ok()?;
            let n: u32 = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("Pages:"))?
                .trim()
                .parse()
                .ok()?;
            let _ = std::fs::write(&count, n.to_string());
            n
        }
    };
    Some(Doc { pdf, dir, pages }).filter(|d| d.pages > 0)
}

/// Page `n` (1-based) as a PNG, rendered on first view.
fn render_page(doc: &Doc, n: u32) -> Option<std::path::PathBuf> {
    let png = doc.dir.join(format!("p{n}.png"));
    if !png.exists() {
        // render under a temporary name: a half-written PNG must never be shown
        let tmp = doc.dir.join(format!("tmp{n}-{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        let mut c = crate::archive::sandboxed("pdftoppm", &doc.pdf, &doc.dir);
        c.args([
            "-singlefile",
            "-scale-to-x",
            "1000",
            "-scale-to-y",
            "-1",
            "-png",
            "-f",
            &n.to_string(),
            "-l",
            &n.to_string(),
        ])
        .arg(&doc.pdf)
        .arg(&tmp);
        let ok = c.output().is_ok_and(|o| o.status.success());
        let made = tmp.with_extension("png");
        if !ok || std::fs::rename(&made, &png).is_err() {
            let _ = std::fs::remove_file(&made);
            return None;
        }
    }
    Some(png)
}

/// One page at a time: arrows, "3 / 12", and the scroll wheel over the page flips pages.
/// Holds its widgets weakly so the handlers that own it don't keep them alive.
struct Pager {
    doc: std::sync::Arc<Doc>,
    cur: std::cell::Cell<u32>,
    pic: glib::WeakRef<gtk::Picture>,
    label: glib::WeakRef<gtk::Label>,
    prev: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    /// placeholder (icon + "Rendering preview…"), hidden once the first page is in
    waiting: glib::WeakRef<gtk::Box>,
    note: glib::WeakRef<gtk::Label>,
    serial: u64,
    me_serial: std::rc::Rc<std::cell::Cell<u64>>,
}

impl Pager {
    /// Show page `n`; false if it's out of range (so a scroll there can scroll the pane instead).
    fn go(self: &std::rc::Rc<Self>, n: u32) -> bool {
        if n < 1 || n > self.doc.pages || n == self.cur.get() {
            return false;
        }
        self.cur.set(n);
        if let (Some(l), Some(p), Some(x)) = (self.label.upgrade(), self.prev.upgrade(), self.next.upgrade()) {
            l.set_label(&format!("{n} / {}", self.doc.pages));
            p.set_sensitive(n > 1);
            x.set_sensitive(n < self.doc.pages);
        }
        let (me, doc) = (self.clone(), self.doc.clone());
        glib::spawn_future_local(async move {
            let png = gio::spawn_blocking(move || render_page(&doc, n)).await.ok().flatten();
            if me.cur.get() != n || me.me_serial.get() != me.serial {
                return; // flipped on, or another file selected, while this page rendered
            }
            match (png, me.pic.upgrade()) {
                (Some(png), Some(pic)) => {
                    pic.set_filename(Some(&png));
                    if let Some(w) = me.waiting.upgrade() {
                        w.set_visible(false);
                    }
                }
                _ => {
                    if let Some(note) = me.note.upgrade() {
                        note.set_label("Couldn't render this page");
                    }
                }
            }
        });
        true
    }
}

/// Markdown as Pango markup: headings, emphasis, code, links, lists, quotes, tables as rows.
/// Where a local picture goes in the rendered text: "\u{1}path\u{1}", split out by md_page. Web images
/// (badges and the like) are left out: a preview shouldn't go online because a file was selected.
fn image_marker(url: &str) -> String {
    if url.contains("://") || url.starts_with("data:") || url.is_empty() {
        String::new()
    } else {
        format!("\u{1}{url}\u{1}")
    }
}

/// The src of every <img> in a piece of HTML.
fn img_srcs(html: &str) -> Vec<String> {
    html.split("<img")
        .skip(1)
        .filter_map(|tag| {
            let tag = tag.split('>').next()?;
            let rest = tag.split("src=").nth(1)?;
            let q = rest.chars().next()?;
            let v = if q == '"' || q == '\'' {
                rest[1..].split(q).next()?
            } else {
                rest.split_whitespace().next()?
            };
            Some(v.to_string())
        })
        .collect()
}

/// Rendered Markdown as a column of text and the local pictures it shows (paths relative to `dir`).
fn md_page(markup: &str, dir: &std::path::Path) -> gtk::Box {
    let col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_start(18)
        .margin_end(18)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    for (i, part) in markup.split('\u{1}').enumerate() {
        if i % 2 == 1 {
            let path = dir.join(glib::Uri::unescape_string(part, None::<&str>).map_or(part.to_string(), |s| s.to_string()));
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::ScaleDown)
                .halign(gtk::Align::Center)
                .build();
            col.append(&pic);
            let pic2 = pic.clone();
            glib::spawn_future_local(async move {
                let f = gio::File::for_path(&path);
                match gio::spawn_blocking(move || load_image(&f)).await {
                    Ok(Some(t)) => {
                        // a scrolled column gives a picture no height of its own: size it from its shape,
                        // fitted to the page (~200 px wide in the preview pane), never enlarged
                        let (w, h) = (t.width().max(1) as f64, t.height() as f64);
                        pic2.set_height_request((h * (200.0 / w).min(1.0)) as i32);
                        pic2.set_paintable(Some(&t));
                    }
                    _ => pic2.set_visible(false),
                }
            });
            continue;
        }
        let text = part.trim_matches('\n');
        if text.trim().is_empty() {
            continue;
        }
        let lbl = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .build();
        // a marker inside a link or heading splits its tags; such a piece shows as plain text
        if gtk::pango::parse_markup(text, '\0').is_ok() {
            lbl.set_markup(text);
        } else {
            lbl.set_text(&strip_tags(text));
        }
        col.append(&lbl);
    }
    col
}

fn md_markup(src: &str) -> String {
    use pulldown_cmark::{Event as E, HeadingLevel as H, Options, Parser, Tag as T, TagEnd as TE};
    let esc = |t: &str| glib::markup_escape_text(t).to_string();
    let (mut out, mut lists) = (String::new(), Vec::<Option<u64>>::new());
    let mut in_image = false; // an image's alt text isn't shown: the picture is
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(src, opts) {
        if in_image {
            in_image = !matches!(ev, E::End(TE::Image));
            continue;
        }
        match ev {
            E::Start(T::Image { dest_url, .. }) => {
                out.push_str(&image_marker(&dest_url));
                in_image = true;
            }
            E::Start(T::Heading { level, .. }) => out.push_str(match level {
                H::H1 => "<span size='xx-large' weight='bold'>",
                H::H2 => "<span size='x-large' weight='bold'>",
                H::H3 => "<span size='large' weight='bold'>",
                _ => "<span weight='bold'>",
            }),
            E::End(TE::Heading(_)) => out.push_str("</span>\n\n"),
            E::End(TE::Paragraph) => out.push_str("\n\n"),
            E::Start(T::Emphasis) | E::Start(T::BlockQuote(_)) => out.push_str("<i>"),
            E::End(TE::Emphasis) => out.push_str("</i>"),
            E::End(TE::BlockQuote(_)) => out.push_str("</i>"),
            E::Start(T::Strong) => out.push_str("<b>"),
            E::End(TE::Strong) => out.push_str("</b>"),
            E::Start(T::Strikethrough) => out.push_str("<s>"),
            E::End(TE::Strikethrough) => out.push_str("</s>"),
            E::Start(T::CodeBlock(_)) => out.push_str("<tt>"),
            E::End(TE::CodeBlock) => out.push_str("</tt>\n"),
            E::Code(t) => out.push_str(&format!("<tt>{}</tt>", esc(&t))),
            E::Start(T::Link { dest_url, .. }) => out.push_str(&format!("<a href=\"{}\">", esc(&dest_url))),
            E::End(TE::Link) => out.push_str("</a>"),
            E::Start(T::List(first)) => {
                // a nested list starts on its own line, not after the parent item's text
                if !lists.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                lists.push(first);
            }
            E::End(TE::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    out.push('\n');
                }
            }
            E::Start(T::Item) => {
                out.push_str(&"    ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("• "),
                }
            }
            E::End(TE::Item) if !out.ends_with('\n') => out.push('\n'),
            E::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            E::End(TE::TableCell) => out.push_str("  │  "),
            E::End(TE::TableHead) | E::End(TE::TableRow) => out.push('\n'),
            E::Text(t) => out.push_str(&esc(&t)),
            // raw HTML (a README's centring <div>, badges): the text between the tags, not the tags
            E::Html(t) | E::InlineHtml(t) => {
                for src in img_srcs(&t) {
                    out.push_str(&image_marker(&src));
                }
                out.push_str(&esc(&strip_tags(&t)));
            }
            E::SoftBreak => out.push(' '),
            E::HardBreak => out.push('\n'),
            E::Rule => out.push_str("────────────\n\n"),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

fn read_head(p: &std::path::Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(p) {
        let _ = f.take(TEXT_LIMIT as u64).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

impl Preview {
    pub fn new() -> Preview {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        // its own titled panel (sidebar shade, border, header) so it never reads as part of the file list
        let header = gtk::CenterBox::builder()
            .start_widget(&gtk::Label::builder().label("Preview").css_classes(["heading"]).build())
            .end_widget(
                &gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .css_classes(["flat", "circular"])
                    .tooltip_text("Close Preview (Alt+P)")
                    .action_name("win.preview")
                    .valign(gtk::Align::Center)
                    .build(),
            )
            .css_classes(["preview-header"])
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(260)
            .visible(false)
            .css_classes(["preview-pane"])
            .build();
        root.append(&header);
        root.append(&scroller);
        crate::label_icon_buttons(root.upcast_ref());
        std::thread::spawn(|| prune(&cache_root(), CACHE_MAX_AGE, CACHE_MAX_BYTES));
        Preview {
            root,
            body,
            serial: Default::default(),
        }
    }

    fn clear(&self) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
    }

    pub fn show(&self, infos: &[gio::FileInfo], git: Option<char>) {
        self.serial.set(self.serial.get() + 1);
        self.clear();
        match infos {
            [] => {
                self.body.append(
                    &adw::StatusPage::builder()
                        .icon_name("view-reveal-symbolic")
                        .title("No selection")
                        .description("Select a file to preview it")
                        .build(),
                );
            }
            [info] => self.one(info, git),
            many => {
                let bytes: u64 = many.iter().filter(|i| i.file_type() != gio::FileType::Directory).map(|i| i.size() as u64).sum();
                self.body.append(
                    &adw::StatusPage::builder()
                        .icon_name("edit-select-all-symbolic")
                        .title(format!("{} items", many.len()))
                        .description(glib::format_size(bytes).as_str())
                        .build(),
                );
            }
        }
    }

    fn one(&self, info: &gio::FileInfo, git: Option<char>) {
        let Some(file) = info_file(info) else { return };
        let ct = crate::browser::content_type(info).map(|c| c.to_string()).unwrap_or_default();
        let is_dir = info.file_type() == gio::FileType::Directory;

        // the big visual
        let visual = gtk::Box::builder().orientation(gtk::Orientation::Vertical).halign(gtk::Align::Fill).build();
        if ct.starts_with("image/") && file.is_native() {
            // decoded off the GTK thread (a 50-megapixel photo takes a while); says why if it can't
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .height_request(260)
                .build();
            visual.append(&pic);
            let (f, pic2) = (file.clone(), pic.clone());
            glib::spawn_future_local(async move {
                match gio::spawn_blocking(move || load_image(&f)).await {
                    Ok(Some(t)) => pic2.set_paintable(Some(&t)),
                    _ => {
                        if let Some(parent) = pic2.parent().and_downcast::<gtk::Box>() {
                            parent.remove(&pic2);
                            parent.append(
                                &gtk::Label::builder()
                                    .label("No preview for this image format")
                                    .css_classes(["dim-label"])
                                    .build(),
                            );
                        }
                    }
                }
            });
        } else if ct == "application/x-openzim"
            && let Some(path) = file.path()
        {
            visual.append(&zim_card(path, info));
        } else if ct.starts_with("audio/") && file.is_native() {
            visual.append(&music(&file, info));
        } else if ct.starts_with("video/") && file.is_native() {
            let v = gtk::Video::for_file(Some(&file));
            v.set_autoplay(false);
            v.set_height_request(220);
            visual.append(&v);
            if let Some(m) = v.media_stream() {
                say_media_errors(&m, &visual);
            }
        } else if (ct == "application/pdf" || ct == "application/illustrator" || is_office(&ct))
            && let Some(path) = file.path()
        {
            // the icon stands in while the document opens and the first page renders
            let waiting = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            let note = gtk::Label::builder().label("Rendering preview…").wrap(true).css_classes(["dim-label"]).build();
            waiting.append(&img);
            waiting.append(&note);
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .css_classes(["card"])
                .build();
            let btn = |icon: &str, tip: &str| {
                gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tip)
                    .css_classes(["flat", "circular"])
                    .sensitive(false)
                    .build()
            };
            let (prev, next) = (btn("go-previous-symbolic", "Previous page"), btn("go-next-symbolic", "Next page"));
            let label = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
            let nav = gtk::CenterBox::builder()
                .start_widget(&prev)
                .center_widget(&label)
                .end_widget(&next)
                .visible(false)
                .build();
            // arrows above the page: pages differ in height, and the buttons must not move under
            // the pointer while clicking through
            visual.append(&nav);
            visual.append(&waiting);
            visual.append(&pic);
            let office = is_office(&ct);
            let stamp = (info.size(), info.modification_date_time().map_or(0, |d| d.to_unix()));
            let (serial, me_serial) = (self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let doc = gio::spawn_blocking(move || open_doc(&path, office, stamp)).await.ok().flatten();
                if me_serial.get() != serial {
                    return;
                }
                let Some(doc) = doc else {
                    note.set_label(if office {
                        "Office previews need LibreOffice (libreoffice-writer, -calc, -impress)"
                    } else {
                        "No preview available"
                    });
                    return;
                };
                nav.set_visible(doc.pages > 1);
                let pager = std::rc::Rc::new(Pager {
                    doc: std::sync::Arc::new(doc),
                    cur: Default::default(),
                    pic: pic.downgrade(),
                    label: label.downgrade(),
                    prev: prev.downgrade(),
                    next: next.downgrade(),
                    waiting: waiting.downgrade(),
                    note: note.downgrade(),
                    serial,
                    me_serial,
                });
                let p = pager.clone();
                prev.connect_clicked(move |_| {
                    p.go(p.cur.get() - 1);
                });
                let p = pager.clone();
                next.connect_clicked(move |_| {
                    p.go(p.cur.get() + 1);
                });
                // wheel/touchpad over the page flips pages; past either end the pane scrolls
                let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
                let p = pager.clone();
                scroll.connect_scroll(move |_, _, dy| {
                    let n = p.cur.get();
                    let to = if dy > 0.0 { n + 1 } else { n.saturating_sub(1) };
                    if p.go(to) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
                });
                pic.add_controller(scroll);
                pager.go(1);
            });
        } else if crate::whiteboard::is_whiteboard(&info.name().to_string_lossy(), &ct)
            && let Some(p) = file.path()
        {
            // a Nextcloud / Excalidraw drawing, drawn on a page like a PDF's
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .height_request(300)
                .css_classes(["card", "md-page"])
                .build();
            visual.append(&pic);
            let pic2 = pic.clone();
            glib::spawn_future_local(async move {
                let drawn = gio::spawn_blocking(move || {
                    let json = std::fs::read_to_string(&p).ok().filter(|j| j.len() < 64 << 20)?;
                    let svg = crate::whiteboard::to_svg(&json)?;
                    gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes())).ok()
                })
                .await;
                match drawn {
                    Ok(Some(t)) => pic2.set_paintable(Some(&t)),
                    _ => pic2.set_alternative_text(Some("Can't draw this whiteboard")),
                }
            });
        } else if gio::content_type_is_a(&ct, "text/markdown")
            && let Some(p) = file.path()
        {
            // rendered onto a page, where PDF and Office documents show theirs, pictures included
            let page = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .height_request(340)
                .css_classes(["card", "md-page"])
                .build();
            visual.append(&page);
            let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
            glib::spawn_future_local(async move {
                if let Ok(text) = gio::spawn_blocking(move || read_head(&p)).await {
                    page.set_child(Some(&md_page(&md_markup(&text), &dir)));
                }
            });
        } else if thumbs::can_thumbnail(info) {
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .height_request(300)
                .build();
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            visual.append(&img);
            visual.append(&pic);
            let (info, serial, me_serial) = (info.clone(), self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let wanted = || me_serial.get() == serial;
                if let Some(p) = thumbs::get(&info, thumbs::Size::XLarge, wanted).await
                    && me_serial.get() == serial
                {
                    pic.set_filename(Some(&p));
                    img.set_visible(false);
                }
            });
        } else {
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            visual.append(&img);
        }
        self.body.append(&visual);
        self.body.append(
            &gtk::Label::builder()
                .label(info.display_name().as_str())
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["title-3"])
                .justify(gtk::Justification::Center)
                .selectable(true)
                .build(),
        );
        if file.is_native() && !is_dir {
            // tag dots act on the previewed file itself (not "the selection", which a click on
            // the list or a refresh may have changed)
            let picker = std::rc::Rc::new(std::cell::OnceCell::<gtk::Box>::new());
            let (p, f) = (picker.clone(), file.clone());
            let row = crate::tags::picker(move |t| {
                if let Err(e) = crate::tags::toggle(std::slice::from_ref(&f), t) {
                    eprintln!("kestrel: tagging failed: {e}");
                }
                if let Some(row) = p.get() {
                    crate::tags::set_picker(row, crate::tags::read(&f));
                }
            });
            crate::tags::set_picker(&row, crate::tags::of_info(info));
            let _ = picker.set(row.clone());
            self.body.append(&row);
        }

        let grid = gtk::Grid::builder().column_spacing(12).row_spacing(6).build();
        let desc = gio::content_type_get_description(&ct);
        row(&grid, 0, "Type", &desc);
        let size_lbl = if is_dir {
            String::new()
        } else {
            glib::format_size(info.size() as u64).to_string()
        };
        row(&grid, 1, "Size", &size_lbl);
        row(&grid, 2, "Modified", &fmt_date(info));
        let loc = file
            .parent()
            .map(|p| p.path().map_or_else(|| p.uri().to_string(), |x| crate::browser::tilde(&x.to_string_lossy())))
            .unwrap_or_default();
        row(&grid, 3, "Location", &loc);
        if info.has_attribute("unix::mode") {
            row(&grid, 4, "Permissions", &perms(info.attribute_uint32("unix::mode")));
        }
        row(&grid, 5, "Owner", info.attribute_string("owner::user").as_deref().unwrap_or(""));
        let git_s = match git {
            Some('M') => "Modified",
            Some('A') => "Added",
            Some('?') => "Untracked",
            Some('D') => "Deleted",
            Some('R') => "Renamed",
            Some('U') => "Conflict",
            _ => "",
        };
        row(&grid, 7, "Git", git_s);
        self.body.append(&grid);

        if is_dir {
            // folder totals straight from the index: instant even for huge trees
            if let Some(p) = file.path() {
                let (serial, me_serial) = (self.serial.get(), self.serial.clone());
                let lbl = gtk::Label::builder().label("Counting…").css_classes(["dim-label"]).build();
                self.body.append(&lbl);
                glib::spawn_future_local(async move {
                    let r = gio::spawn_blocking(move || crate::search::folder_stats(&p))
                        .await
                        .unwrap_or(Err("Counting failed"));
                    if me_serial.get() == serial {
                        lbl.set_label(&match r {
                            Ok((n, b)) => format!("{n} items inside, {}", glib::format_size(b)),
                            Err(why) => why.into(),
                        });
                    }
                });
            }
        } else if crate::whiteboard::is_whiteboard(&info.name().to_string_lossy(), &ct) {
            // drawn at the top (it's JSON, so text/plain would dump it here)
        } else if gio::content_type_is_a(&ct, "text/markdown") {
            // shown as a page at the top (and markdown is a text/plain, so it must stop here)
        } else if let Some(p) = file.path() {
            // decided by the content, not the suffix: .vhd is a disk image or VHDL source, .ts MPEG
            // video or TypeScript, and GIO types many text formats (.pem, ASCII .dxf/.stl…) as
            // binary by name (search results aren't sniffed at all)
            let archive = crate::archive::archive_ext(&info.name().to_string_lossy()).is_some();
            let lbl = gtk::Label::builder()
                .label("Reading archive…")
                .xalign(0.0)
                .wrap(true)
                .visible(archive)
                .css_classes(["dim-label"])
                .build();
            self.body.append(&lbl);
            let (body, grid, serial, me_serial) = (self.body.clone(), grid.clone(), self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let r = gio::spawn_blocking(move || {
                    if let Some(t) = sniff_text(&p) {
                        return (None, Ok(Body::Text(t)));
                    }
                    let kind = binary_kind(&read_bytes(&p, 4096));
                    if !archive {
                        return (kind, Ok(Body::Nothing));
                    }
                    let list = p.canonicalize().map_err(|e| e.to_string()).and_then(|p| {
                        let scratch = cache_root().join("list");
                        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
                        crate::archive::list_archive(&p, &scratch)
                    });
                    (kind, list.map(|e| archive_text(&e)).map(|(sum, l)| Body::Listing(sum, l)))
                })
                .await
                .unwrap_or((None, Err("Couldn't read this archive".into())));
                if me_serial.get() != serial {
                    return;
                }
                if let Some(k) = r.0 {
                    row(&grid, 6, "Format", &k);
                }
                match r.1 {
                    Ok(Body::Text(text)) => {
                        lbl.set_visible(false);
                        if text.contains("PRIVATE KEY-----") {
                            body.append(
                                &gtk::Label::builder()
                                    .label("Private key: contents not shown")
                                    .xalign(0.0)
                                    .css_classes(["dim-label"])
                                    .build(),
                            );
                        } else {
                            body.append(&text_card(&text));
                        }
                    }
                    Ok(Body::Listing(summary, list)) => {
                        lbl.set_label(&summary);
                        body.append(&text_card(&list));
                    }
                    Ok(Body::Nothing) => {}
                    Err(e) => lbl.set_label(&e),
                }
            });
        }
    }
}

/// What goes under the details: the file as text, or an archive's summary and listing.
enum Body {
    Text(String),
    Listing(String, String),
    Nothing,
}

fn read_bytes(p: &std::path::Path, n: u64) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(p) {
        let _ = f.take(n).read_to_end(&mut buf);
    }
    buf
}

/// What a binary really is, from its magic bytes (the suffix can lie, and ELF programs have none).
fn binary_kind(h: &[u8]) -> Option<String> {
    let u16le = |o: usize| h.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32le = |o: usize| h.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u32be = |o: usize| h.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    if h.starts_with(b"\x7fELF") {
        let bits = if h.get(4) == Some(&2) { "64-bit" } else { "32-bit" };
        let be = h.get(5) == Some(&2);
        let rd = |o: usize| {
            h.get(o..o + 2).map(|b| {
                if be {
                    u16::from_be_bytes([b[0], b[1]])
                } else {
                    u16::from_le_bytes([b[0], b[1]])
                }
            })
        };
        let what = match rd(16)? {
            1 => "object file (.o, kernel module)",
            2 => "program",
            3 => "program or shared library",
            4 => "core dump",
            _ => "file",
        };
        let arch = match rd(18)? {
            3 => "x86",
            0x3e => "x86-64",
            0x28 => "ARM",
            0xb7 => "ARM64",
            0xf3 => "RISC-V",
            8 => "MIPS",
            0x14 => "PowerPC",
            0x15 => "PowerPC 64",
            0x5e => "Xtensa (ESP32)",
            0xf7 => "BPF",
            _ => "other CPU",
        };
        // an AppImage's runtime marks itself in the ELF padding
        let app = if h.get(8..11) == Some(b"AI\x02") { "AppImage, " } else { "" };
        return Some(format!("{app}Linux {what}, {bits} {arch}"));
    }
    if h.starts_with(b"MZ") {
        let pe = u32le(0x3c)? as usize;
        if h.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Some("DOS program".into());
        }
        let arch = match u16le(pe + 4)? {
            0x14c => "x86",
            0x8664 => "x86-64",
            0xaa64 => "ARM64",
            0x1c4 => "ARM",
            _ => "other CPU",
        };
        let what = match (u16le(pe + 24 + 68), u16le(pe + 22).unwrap_or(0) & 0x2000 != 0) {
            (Some(10..=13), _) => "UEFI program",
            (Some(1), _) => "Windows driver",
            (_, true) => "Windows library (DLL)",
            _ => "Windows program",
        };
        return Some(format!("{what}, {arch}"));
    }
    match u32be(0)? {
        0xfeedfacf | 0xcffaedfe => return Some("macOS program or library, 64-bit".into()),
        0xfeedface | 0xcefaedfe => return Some("macOS program or library, 32-bit".into()),
        // shared by fat Mach-O and Java: a class file's version is ≥ 45, a fat binary's arch count tiny
        0xcafebabe if u32be(4)? >= 45 => return Some(format!("Java class (Java {})", u16::from_be_bytes([h[6], h[7]]).saturating_sub(44))),
        0xcafebabe => return Some("macOS universal program or library".into()),
        _ => {}
    }
    let kind = if h.starts_with(b"\0asm") {
        "WebAssembly module"
    } else if h.starts_with(b"UF2\n") {
        "UF2 firmware (microcontroller)"
    } else if h.starts_with(b"!<arch>\n") {
        "ar archive (static library or package)"
    } else if h.starts_with(b"FOR1") && h.get(8..12) == Some(b"BEAM") {
        "Erlang/Elixir bytecode"
    } else if h.starts_with(b"\0\x09\x0f\xf0\x0f\xf0\x0f\xf0\x0f\xf0\0\0\x01") {
        "FPGA bitstream (Xilinx)"
    } else if h.get(2..4) == Some(b"\r\n") && u16le(0).is_some_and(|m| (3000..4000).contains(&m)) {
        "Python bytecode"
    } else {
        return None;
    };
    Some(kind.into())
}

/// The file's head if it's text (UTF-8, no NULs, not empty), else None.
fn sniff_text(p: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(p).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    // a multi-byte character cut off at the 4 KiB mark is still text
    let utf8 = std::str::from_utf8(&buf).map_or_else(|e| e.error_len().is_none(), |_| true);
    (!buf.is_empty() && !buf.contains(&0) && utf8).then(|| read_head(p))
}

/// Monospace, read-only, scrollable text in a card.
fn text_card(text: &str) -> gtk::ScrolledWindow {
    let tv = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(8)
        .left_margin(8)
        .bottom_margin(8)
        .right_margin(8)
        .build();
    tv.buffer().set_text(text);
    tv.add_css_class("card");
    gtk::ScrolledWindow::builder().child(&tv).height_request(360).vexpand(true).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_drops_stale_then_oldest() {
        let root = std::env::temp_dir().join(format!("kestrel-prune-{}", std::process::id()));
        let day = std::time::Duration::from_secs(86400);
        let now = std::time::SystemTime::now();
        for (name, age, bytes) in [("old", 40, 1), ("mid", 5, 600), ("new", 1, 600)] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("p1.png"), vec![0u8; bytes]).unwrap();
            std::fs::File::open(root.join(name)).unwrap().set_modified(now - day * age).unwrap();
        }
        prune(&root, day * 30, 1000);
        let left: Vec<_> = ["old", "mid", "new"].into_iter().filter(|n| root.join(n).exists()).collect();
        assert_eq!(left, ["new"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn binary_kinds() {
        let mut elf = b"\x7fELF\x02\x01\x01\0AI\x02\0\0\0\0\0\x02\0\x3e\0".to_vec();
        assert_eq!(binary_kind(&elf).as_deref(), Some("AppImage, Linux program, 64-bit x86-64"));
        elf[8] = 0;
        elf[16] = 1;
        elf[18] = 0xb7;
        assert_eq!(binary_kind(&elf).as_deref(), Some("Linux object file (.o, kernel module), 64-bit ARM64"));
        let mut pe = vec![0u8; 0x200];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x80;
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        pe[0x80 + 22..0x80 + 24].copy_from_slice(&0x2000u16.to_le_bytes());
        assert_eq!(binary_kind(&pe).as_deref(), Some("Windows library (DLL), x86-64"));
        pe[0x80 + 92] = 10;
        assert_eq!(binary_kind(&pe).as_deref(), Some("UEFI program, x86-64"));
        assert_eq!(binary_kind(b"\xca\xfe\xba\xbe\0\0\0\x41").as_deref(), Some("Java class (Java 21)"));
        assert_eq!(
            binary_kind(b"\xca\xfe\xba\xbe\0\0\0\x02").as_deref(),
            Some("macOS universal program or library")
        );
        assert_eq!(binary_kind(b"\0asm\x01\0\0\0").as_deref(), Some("WebAssembly module"));
        assert_eq!(binary_kind(b"\xcb\x0d\r\n").as_deref(), Some("Python bytecode"));
        assert!(binary_kind(b"PK\x03\x04").is_none());
        // real files on this machine, when present
        for (f, want) in [
            ("/usr/bin/true", "Linux program or shared library, 64-bit x86-64"),
            ("/usr/lib/kestrel/7zz", "Linux program"),
        ] {
            if let Ok(b) = std::fs::read(f) {
                assert!(binary_kind(&b[..4096.min(b.len())]).is_some_and(|k| k.contains(want)), "{f}");
            }
        }
        assert!(binary_kind(b"").is_none());
    }

    #[test]
    fn sniffs_text() {
        let d = std::env::temp_dir().join(format!("kestrel-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = |n: &str, b: &[u8]| {
            std::fs::write(d.join(n), b).unwrap();
            sniff_text(&d.join(n))
        };
        assert_eq!(w("a.dxf", b"0\nSECTION\n").as_deref(), Some("0\nSECTION\n"));
        let cut = format!("a{}", "é".repeat(2048)); // the 4 KiB read ends mid-character
        assert!(w("b.json", cut.as_bytes()).is_some());
        assert!(w("c.stl", b"\x80\x00binary").is_none());
        assert!(w("d.bin", &[0xff, 0xfe, 0x41]).is_none());
        assert!(w("e.txt", b"").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn markdown_is_valid_pango() {
        let md = "# T <x>\n\nSome **bold** & *it* `c<d>` [link](http://a?b=1&c=2)\n\n- a\n  1. one\n  2. two\n- [x] done\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nfn x() {}\n```\n---\n";
        let m = md_markup(md);
        // <a href> is GtkLabel's extension to Pango markup; check the rest parses
        let (pre, post) = m.split_once("<a href=").unwrap();
        let plain = format!("{pre}{}", post.split_once('>').unwrap().1.replacen("</a>", "", 1));
        gtk::pango::parse_markup(&plain, '\0').unwrap();
        assert!(m.contains("<b>bold</b>") && m.contains("2. two") && m.contains("☑") && m.contains("c&lt;d&gt;"));
        // raw HTML: tags dropped, text kept
        assert!(!m.contains("&lt;x&gt;"));
        let h = md_markup("<div align=\"center\">\n<img src=\"a.svg\" alt=\"icon\">\n\nHello <b>there</b>\n</div>\n");
        assert!(!h.contains("div") && !h.contains("<img") && h.contains("there"), "{h}");
        // local pictures are marked for md_page, web ones (badges) left out
        assert!(h.contains("\u{1}a.svg\u{1}"), "{h}");
        let m = md_markup("![Logo](docs/logo.png) and ![badge](https://img.shields.io/x)\n");
        assert!(
            m.contains("\u{1}docs/logo.png\u{1}") && !m.contains("shields") && !m.contains("Logo") && !m.contains("badge"),
            "{m}"
        );
        assert!(is_office("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(!is_office("application/pdf"));
    }
}
