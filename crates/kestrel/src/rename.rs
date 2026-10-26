//! Batch rename: find/replace (text or regex), change case, add numbers. Works on the name
//! without its extension; a live preview flags clashes before anything is touched.

use crate::fileops::{Ops, Undo};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Keep,
    Lower,
    Upper,
    Title,
}

pub struct Rule {
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub case: Case,
    /// Some((start, digits)): append " 001", " 002"…
    pub number: Option<(u32, usize)>,
}

/// "photo.tar.gz" → ("photo", ".tar.gz")? No: only the last extension, like Nautilus
/// ("photo.tar", ".gz"); a leading dot isn't an extension.
fn split(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase())
        } else {
            out.extend(c.to_lowercase())
        }
        start = !c.is_alphanumeric();
    }
    out
}

/// The new names, in order; Err for a bad regex.
pub fn apply(names: &[String], r: &Rule) -> Result<Vec<String>, String> {
    let re = if r.regex && !r.find.is_empty() {
        Some(regex::Regex::new(&r.find).map_err(|e| e.to_string())?)
    } else {
        None
    };
    Ok(names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let (stem, ext) = split(name);
            let mut s = match &re {
                Some(re) => re.replace_all(stem, r.replace.as_str()).into_owned(),
                None if !r.find.is_empty() => stem.replace(&r.find, &r.replace),
                None => stem.to_string(),
            };
            s = match r.case {
                Case::Keep => s,
                Case::Lower => s.to_lowercase(),
                Case::Upper => s.to_uppercase(),
                Case::Title => title_case(&s),
            };
            if let Some((start, digits)) = r.number {
                let n = format!("{:0digits$}", start as usize + i);
                s = if s.is_empty() { n } else { format!("{s} {n}") };
            }
            s + ext
        })
        .collect())
}

/// Why `new` can't be used, if it can't. `taken`: names already in its folder (outside the batch)
/// and names earlier items in the batch are getting.
fn problem(new: &str, taken: &std::collections::HashSet<String>) -> Option<&'static str> {
    if new.is_empty() || new == "." || new == ".." {
        Some("empty name")
    } else if new.contains('/') {
        Some("contains /")
    } else if taken.contains(new) {
        Some("name already used")
    } else {
        None
    }
}

/// Rename in an order that never clobbers: an item whose new name is still held by another item
/// of the batch waits for it; a cycle (a↔b) goes through a temporary name. Returns (old, new)
/// pairs done and the first error.
fn rename_all(jobs: Vec<(gio::File, String)>) -> (Vec<(gio::File, gio::File)>, Option<String>) {
    let mut pending: Vec<(gio::File, String)> = jobs
        .into_iter()
        .filter(|(f, n)| f.basename().is_some_and(|b| b.to_string_lossy() != *n))
        .collect();
    let mut done = Vec::new();
    while !pending.is_empty() {
        let busy = |f: &gio::File, n: &str| f.parent().is_some_and(|d| d.child(n).query_exists(gio::Cancellable::NONE));
        let Some(i) = pending.iter().position(|(f, n)| !busy(f, n)) else {
            // every target is taken: fine if by another item still to move (a cycle, a↔b): park
            // one of those under a temporary name and the rest can move; otherwise it's a real clash
            let held = |g: &gio::File| pending.iter().any(|(f, n)| f.parent().is_some_and(|d| d.child(n).equal(g)));
            let Some(k) = pending.iter().position(|(g, _)| held(g)) else {
                return (done, Some(format!("{}: already exists", pending[0].1)));
            };
            let (f, n) = pending.remove(k);
            let tmp = format!(".kestrel-rename-{}", std::process::id());
            match f.set_display_name(&tmp, gio::Cancellable::NONE) {
                Ok(t) => {
                    done.push((f, t.clone()));
                    pending.push((t, n));
                }
                Err(e) => return (done, Some(crate::browser::human_gio(&e))),
            }
            continue;
        };
        let (f, n) = pending.remove(i);
        match f.set_display_name(&n, gio::Cancellable::NONE) {
            Ok(new) => done.push((f, new)),
            Err(e) => return (done, Some(format!("{n}: {}", crate::browser::human_gio(&e)))),
        }
    }
    (done, None)
}

/// Folder URI → names already in it that aren't part of the batch (they'd clash with a new name).
type Others = std::collections::HashMap<String, std::collections::HashSet<String>>;

fn folder_key(f: &gio::File) -> String {
    f.parent().map(|d| d.uri().to_string()).unwrap_or_default()
}

/// Read each folder once, on a worker thread (a slow network folder must not freeze typing).
fn others_in_folders(files: &[gio::File]) -> Others {
    let mut out = Others::new();
    for f in files {
        let key = folder_key(f);
        if out.contains_key(&key) {
            continue;
        }
        let names = f
            .parent()
            .and_then(|d| d.path())
            .and_then(|p| std::fs::read_dir(p).ok())
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        out.insert(key, names);
    }
    for f in files {
        if let (Some(set), Some(n)) = (out.get_mut(&folder_key(f)), f.basename()) {
            set.remove(&*n.to_string_lossy());
        }
    }
    out
}

pub fn show(ops: &Rc<Ops>, files: Vec<gio::File>) {
    let ops = ops.clone();
    glib::spawn_future_local(async move {
        let f2 = files.clone();
        if let Ok(others) = gio::spawn_blocking(move || others_in_folders(&f2)).await {
            open(&ops, files, others);
        }
    });
}

fn open(ops: &Rc<Ops>, files: Vec<gio::File>, others: Others) {
    let names: Vec<String> = files.iter().map(|f| f.basename().unwrap_or_default().to_string_lossy().into_owned()).collect();
    let find = adw::EntryRow::builder().title("Find").build();
    let replace = adw::EntryRow::builder().title("Replace with").build();
    let regex = adw::SwitchRow::builder().title("Regular expression").subtitle("Use $1, $2… for groups").build();
    let case = adw::ComboRow::builder()
        .title("Case")
        .model(&gtk::StringList::new(&["Keep", "lowercase", "UPPERCASE", "Title Case"]))
        .build();
    let number = adw::SwitchRow::builder().title("Add numbers").build();
    let start = adw::SpinRow::with_range(0.0, 1_000_000.0, 1.0);
    start.set_title("Start at");
    start.set_value(1.0);
    let digits = adw::SpinRow::with_range(1.0, 9.0, 1.0);
    digits.set_title("Digits");
    digits.set_value(3.0);
    number.bind_property("active", &start, "sensitive").sync_create().build();
    number.bind_property("active", &digits, "sensitive").sync_create().build();
    let g1 = adw::PreferencesGroup::builder()
        .title("Find and Replace")
        .description("Applies to the name, not the extension")
        .build();
    for w in [find.upcast_ref::<gtk::Widget>(), replace.upcast_ref(), regex.upcast_ref()] {
        g1.add(w);
    }
    let g2 = adw::PreferencesGroup::builder().title("Format").build();
    for w in [case.upcast_ref::<gtk::Widget>(), number.upcast_ref(), start.upcast_ref(), digits.upcast_ref()] {
        g2.add(w);
    }
    let preview = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let g3 = adw::PreferencesGroup::builder().title("Preview").build();
    g3.add(&preview);
    let page = adw::PreferencesPage::new();
    for g in [&g1, &g2, &g3] {
        page.add(g);
    }
    let go = gtk::Button::builder().label("Rename").css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::new();
    header.pack_end(&go);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(format!("Rename {} Items", files.len()))
        .child(&tv)
        .content_width(560)
        .content_height(680)
        .build();

    let plan: Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let update = {
        let (find, replace, regex, case, number, start, digits) = (
            find.clone(),
            replace.clone(),
            regex.clone(),
            case.clone(),
            number.clone(),
            start.clone(),
            digits.clone(),
        );
        let (preview, go, plan, files, names) = (preview.clone(), go.clone(), plan.clone(), files.clone(), names.clone());
        Rc::new(move || {
            while let Some(c) = preview.first_child() {
                preview.remove(&c);
            }
            let rule = Rule {
                find: find.text().to_string(),
                replace: replace.text().to_string(),
                regex: regex.is_active(),
                case: [Case::Keep, Case::Lower, Case::Upper, Case::Title][case.selected().min(3) as usize],
                number: number.is_active().then(|| (start.value() as u32, digits.value() as usize)),
            };
            let new = match apply(&names, &rule) {
                Ok(n) => n,
                Err(e) => {
                    preview.append(
                        &adw::ActionRow::builder()
                            .title("Invalid regular expression")
                            .subtitle(e)
                            .css_classes(["error"])
                            .build(),
                    );
                    go.set_sensitive(false);
                    return;
                }
            };
            // names already in each folder that aren't part of this batch, plus the batch's new names
            // names already used in each folder: outside the batch, plus earlier new names
            let mut taken = others.clone();
            let mut ok = true;
            for (i, (f, n)) in files.iter().zip(&new).enumerate() {
                let set = taken.entry(folder_key(f)).or_default();
                let bad = problem(n, set);
                set.insert(n.clone());
                ok &= bad.is_none();
                if i < 200 || bad.is_some() {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(n).as_str())
                        .subtitle(glib::markup_escape_text(&names[i]).as_str())
                        .build();
                    if let Some(why) = bad {
                        row.set_subtitle(&format!("{} · {why}", glib::markup_escape_text(&names[i])));
                        row.add_css_class("error");
                    }
                    preview.append(&row);
                }
            }
            go.set_sensitive(ok && new != names);
            *plan.borrow_mut() = new;
        })
    };
    update();
    let u = update.clone();
    find.connect_changed(move |_| u());
    let u = update.clone();
    replace.connect_changed(move |_| u());
    for w in [&regex, &number] {
        let u = update.clone();
        w.connect_active_notify(move |_| u());
    }
    let u = update.clone();
    case.connect_selected_notify(move |_| u());
    for w in [&start, &digits] {
        let u = update.clone();
        w.connect_value_notify(move |_| u());
    }
    let parent = ops.window();
    let (ops, d) = (ops.clone(), dialog.clone());
    go.connect_clicked(move |_| {
        d.close();
        let jobs: Vec<(gio::File, String)> = files.iter().cloned().zip(plan.borrow().iter().cloned()).collect();
        let n = jobs.len();
        ops.job(
            format!("Renaming {n} items"),
            move |_| rename_all(jobs),
            move |ops, (done, err)| {
                if let Some(e) = err {
                    ops.toast(&format!("Rename stopped: {e}"));
                }
                if !done.is_empty() {
                    // undone last-first, so chains and cycles unwind in reverse
                    let undo = Undo::Many(done.iter().rev().map(|(old, new)| Undo::Renamed(old.clone(), new.clone())).collect());
                    ops.toast_undo(&format!("Renamed {n} items"), undo);
                }
            },
        );
    });
    dialog.present(parent.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Rule {
        Rule {
            find: String::new(),
            replace: String::new(),
            regex: false,
            case: Case::Keep,
            number: None,
        }
    }

    #[test]
    fn rules() {
        let names: Vec<String> = ["IMG_0001.JPG", "IMG_0002.JPG", ".hidden", "notes"].map(String::from).to_vec();
        let r = Rule {
            find: "IMG_".into(),
            replace: "Holiday ".into(),
            ..rule()
        };
        assert_eq!(apply(&names, &r).unwrap(), ["Holiday 0001.JPG", "Holiday 0002.JPG", ".hidden", "notes"]);
        let r = Rule {
            find: r"^IMG_(\d+)$".into(),
            replace: "photo-$1".into(),
            regex: true,
            case: Case::Upper,
            ..rule()
        };
        assert_eq!(apply(&names, &r).unwrap()[0], "PHOTO-0001.JPG");
        let r = Rule {
            find: ".*".into(),
            replace: "Trip".into(),
            regex: true,
            number: Some((1, 3)),
            ..rule()
        };
        assert_eq!(apply(&names[..2], &r).unwrap(), ["Trip 001.JPG", "Trip 002.JPG"]);
        let r = Rule { case: Case::Title, ..rule() };
        assert_eq!(apply(&["my old_file-name.txt".to_string()], &r).unwrap(), ["My Old_File-Name.txt"]);
        assert!(
            apply(
                &names,
                &Rule {
                    find: "(".into(),
                    regex: true,
                    ..rule()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn clashes_count_only_files_outside_the_batch() {
        let d = std::env::temp_dir().join(format!("kestrel-others-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(d.join(n), "").unwrap();
        }
        let batch = [gio::File::for_path(d.join("a")), gio::File::for_path(d.join("b"))];
        let others = others_in_folders(&batch);
        let set = &others[&folder_key(&batch[0])];
        // a↔b is fine (both are being renamed), but c belongs to a file outside the batch
        assert!(problem("b", set).is_none() && problem("a", set).is_none());
        assert_eq!(problem("c", set), Some("name already used"));
        assert_eq!(others.len(), 1, "both files share one folder entry");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn swaps_and_chains() {
        let d = std::env::temp_dir().join(format!("kestrel-ren-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (n, c) in [("a", "A"), ("b", "B"), ("c", "C")] {
            std::fs::write(d.join(n), c).unwrap();
        }
        let f = |n: &str| gio::File::for_path(d.join(n));
        // a↔b swap, then a chain (a→c while c→d)
        let (_, err) = rename_all(vec![(f("a"), "b".into()), (f("b"), "a".into())]);
        assert!(err.is_none());
        assert_eq!(std::fs::read_to_string(d.join("a")).unwrap(), "B");
        let (_, err) = rename_all(vec![(f("a"), "c".into()), (f("c"), "d".into())]);
        assert!(err.is_none());
        // a name held by a file outside the batch is an error, not a hang
        let (_, err) = rename_all(vec![(f("c"), "d".into())]);
        assert_eq!(err.as_deref(), Some("d: already exists"));
        assert_eq!(std::fs::read_to_string(d.join("c")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(d.join("d")).unwrap(), "C");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
