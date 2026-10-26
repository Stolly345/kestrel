//! Terminal panel (Ctrl+`) under the file views, kept in step with them: browsing to a folder
//! cds the shell there, and `cd` in the shell moves the view. The shell's folder is read from
//! /proc (works for bash, zsh, fish without any shell setup); a `cd` is only sent while the
//! shell sits at its prompt and the terminal doesn't have the keyboard.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use vte4::prelude::*;

pub struct Term {
    /// File views on top, terminal below; this replaces the views as the window's content.
    pub paned: gtk::Paned,
    shell: RefCell<Option<(vte4::Terminal, i32)>>,
    /// The folder both sides last agreed on.
    synced: RefCell<PathBuf>,
    poll: RefCell<Option<glib::SourceId>>,
    on_cd: Box<dyn Fn(PathBuf)>,
}

fn shell_cmd() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".into())
}

/// `cd` the shell will run as typed: leading space keeps it out of history, Ctrl+U clears
/// anything half-typed first.
fn cd_line(dir: &Path) -> Vec<u8> {
    let quoted = dir.to_string_lossy().replace('\'', r"'\''");
    format!("\x15 cd -- '{quoted}'\r").into_bytes()
}

impl Term {
    pub fn new(views: &impl IsA<gtk::Widget>, on_cd: impl Fn(PathBuf) + 'static) -> Rc<Term> {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(views)
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_end_child(false)
            .build();
        Rc::new(Term {
            paned,
            shell: RefCell::default(),
            synced: RefCell::default(),
            poll: RefCell::default(),
            on_cd: Box::new(on_cd),
        })
    }

    pub fn is_open(&self) -> bool {
        self.paned.end_child().is_some_and(|c| c.is_visible())
    }

    pub fn toggle(self: &Rc<Self>, dir: Option<PathBuf>) {
        if self.is_open() {
            self.close();
            return;
        }
        let term = self.shell.borrow().as_ref().map(|(t, _)| t.clone());
        let term = term.unwrap_or_else(|| self.spawn(dir.clone()));
        if self.paned.end_child().is_none() {
            let sw = gtk::ScrolledWindow::builder().child(&term).height_request(120).build();
            self.paned.set_end_child(Some(&sw));
            let h = self.paned.height();
            self.paned.set_position(if h > 400 { h - 260 } else { h * 2 / 3 });
        }
        if let Some(c) = self.paned.end_child() {
            c.set_visible(true);
        }
        term.grab_focus();
        self.start_polling();
        self.follow(dir);
    }

    fn close(&self) {
        if let Some(c) = self.paned.end_child() {
            c.set_visible(false);
        }
        if let Some(id) = self.poll.take() {
            id.remove();
        }
    }

    fn spawn(self: &Rc<Self>, dir: Option<PathBuf>) -> vte4::Terminal {
        let term = vte4::Terminal::builder().scrollback_lines(10_000).vexpand(true).build();
        // VTE is black whatever the theme; follow light/dark like the views around it
        let style = adw::StyleManager::default();
        let colors = |t: &vte4::Terminal, dark: bool| {
            let (fg, bg) = if dark { ("#ffffff", "#1e1e1e") } else { ("#2e3436", "#ffffff") };
            t.set_colors(gtk::gdk::RGBA::parse(fg).ok().as_ref(), gtk::gdk::RGBA::parse(bg).ok().as_ref(), &[]);
        };
        colors(&term, style.is_dark());
        let weak = term.downgrade();
        let id = style.connect_dark_notify(move |s| {
            if let Some(t) = weak.upgrade() {
                colors(&t, s.is_dark());
            }
        });
        let id = RefCell::new(Some(id));
        term.connect_destroy(move |_| {
            if let Some(id) = id.take() {
                adw::StyleManager::default().disconnect(id);
            }
        });
        let sh = shell_cmd();
        let wd = dir.as_ref().map(|d| d.to_string_lossy().into_owned());
        *self.synced.borrow_mut() = dir.unwrap_or_default();
        let me = Rc::downgrade(self);
        let t2 = term.clone();
        term.spawn_async(
            vte4::PtyFlags::DEFAULT,
            wd.as_deref(),
            &[&sh],
            &[],
            glib::SpawnFlags::DEFAULT,
            || {},
            -1,
            gio::Cancellable::NONE,
            move |r| match (r, me.upgrade()) {
                (Ok(pid), Some(me)) => *me.shell.borrow_mut() = Some((t2.clone(), pid.0)),
                (Err(e), _) => t2.feed(format!("Couldn't start {e}\r\n").as_bytes()),
                _ => {}
            },
        );
        // `exit` closes the panel; the next Ctrl+` starts a fresh shell
        let me = Rc::downgrade(self);
        term.connect_child_exited(move |_, _| {
            if let Some(me) = me.upgrade() {
                me.close();
                me.shell.take();
                me.paned.set_end_child(None::<&gtk::Widget>);
            }
        });
        term
    }

    /// The shell's pid if it's at its prompt (it owns the terminal's foreground), else None.
    fn idle_shell(&self) -> Option<i32> {
        let shell = self.shell.borrow();
        let (t, pid) = shell.as_ref()?;
        let pty = t.pty()?;
        let fg = unsafe { libc::tcgetpgrp(std::os::fd::AsRawFd::as_raw_fd(&pty.fd())) };
        (fg == *pid).then_some(*pid)
    }

    /// The views moved to `dir`: take the shell there too.
    pub fn follow(&self, dir: Option<PathBuf>) {
        let Some(dir) = dir else { return };
        if !self.is_open() || *self.synced.borrow() == dir || self.idle_shell().is_none() {
            return;
        }
        let shell = self.shell.borrow();
        let Some((t, _)) = shell.as_ref() else { return };
        if t.has_focus() {
            return; // they're typing in it
        }
        t.feed_child(&cd_line(&dir));
        *self.synced.borrow_mut() = dir;
    }

    fn start_polling(self: &Rc<Self>) {
        if self.poll.borrow().is_some() {
            return;
        }
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            let Some(me) = me.upgrade() else { return glib::ControlFlow::Break };
            if let Some(pid) = me.idle_shell()
                && let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd"))
                && *me.synced.borrow() != cwd
            {
                *me.synced.borrow_mut() = cwd.clone();
                (me.on_cd)(cwd);
            }
            glib::ControlFlow::Continue
        });
        *self.poll.borrow_mut() = Some(id);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cd_quoting() {
        assert_eq!(
            super::cd_line(std::path::Path::new("/home/a/it's here")),
            b"\x15 cd -- '/home/a/it'\\''s here'\r"
        );
    }
}
