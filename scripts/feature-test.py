#!/usr/bin/env python3
"""Feature test: drives the real Kestrel the way a person does (clicks, keys, typing) and checks
what actually happened, on screen (through the accessibility tree) and on disk.

    scripts/feature-test.py              # every test
    scripts/feature-test.py rename       # only tests whose name contains "rename"

Runs under a virtual X display with a throwaway home folder, a demo folder rebuilt for every
test, and a private search service over that folder; nothing of yours is touched. Archives are
covered elsewhere (cargo test every_format); live index updates and permission filtering need
root and are in scripts/smoke-live.sh. Failed tests leave a screenshot in target/feature-test/.

Needs: xvfb, python3-xlib, gir1.2-atspi-2.0 (all in Ubuntu by default except xvfb).
"""
import hashlib, os, shutil, subprocess, sys, tempfile, time, traceback, zlib, struct

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "target", "feature-test")
BIN = os.path.join(ROOT, "target", "release")

# ── outer: build, make a sandbox, re-run ourselves under Xvfb + a private D-Bus session ──
if os.environ.get("KESTREL_FT_INNER") != "1":
    if not shutil.which("xvfb-run"):
        sys.exit("needs xvfb: sudo apt install xvfb")
    r = subprocess.run(["cargo", "build", "--release", "-q", "-p", "kestrel", "-p", "kestrel-indexd", "-p", "ks"], cwd=ROOT)
    if r.returncode:
        sys.exit(r.returncode)
    shutil.rmtree(OUT, ignore_errors=True)
    os.makedirs(OUT)
    home = tempfile.mkdtemp(prefix="kestrel-ft.")
    run = tempfile.mkdtemp(prefix="kestrel-ft-run.")  # outside HOME, or GIO lists its FUSE mounts as drives
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR=run, GDK_BACKEND="x11", GDK_DEBUG="no-portals",
               KESTREL_FT_INNER="1", KESTREL_SOCK=os.path.join(run, "kestrel.sock"), NO_AT_BRIDGE="0")
    for v in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_CURRENT_DESKTOP"):
        env.pop(v, None)
    try:
        r = subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1440x900x24", "dbus-run-session", "--",
                            sys.executable, "-I", os.path.abspath(__file__), *sys.argv[1:]],
                           env=env, stdin=subprocess.DEVNULL, stderr=open(os.path.join(OUT, "session.log"), "w"))
    finally:
        subprocess.run(["chmod", "-R", "u+rwx", home], stderr=subprocess.DEVNULL)
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(run, ignore_errors=True)
    sys.exit(r.returncode)

# ── inner ──
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
from Xlib import X, XK, display
from Xlib.ext import xtest

HOME = os.environ["HOME"]
RUN = os.environ["XDG_RUNTIME_DIR"]
D = os.path.join(HOME, "Demo")
TRASH = os.path.join(HOME, ".local/share/Trash/files")
CONF = os.path.join(HOME, ".config/kestrel")
dpy = display.Display()
SHOWING = Atspi.StateType.SHOWING


def demo():
    """A fresh demo folder (and trash, settings, session) for every test."""
    for p in (D, os.path.join(HOME, ".local/share/Trash"), CONF, os.path.join(HOME, ".config/gtk-3.0")):
        subprocess.run(["chmod", "-R", "u+rwx", p], stderr=subprocess.DEVNULL)
        shutil.rmtree(p, ignore_errors=True)
    os.makedirs(os.path.join(D, "Projects/aurora/src"))
    w = lambda p, s: open(os.path.join(D, p), "w").write(s)
    w("notes.md", "# Aurora\n\nA tiny **weather station** dashboard.\n")
    w("Projects/aurora/src/main.rs", 'fn main() {\n    println!("aurora");\n}\n')
    for n in ("IMG_0001.txt", "IMG_0002.txt", "IMG_0003.txt"):
        w(n, n + "\n")
    w("budget.xlsx", "not really a spreadsheet\n")
    w("report.pdf", "%PDF-1.4\n%%EOF\n")
    w(".hidden-file", "shh\n")
    os.setxattr(os.path.join(D, "IMG_0002.txt"), "user.chimera.tags", b"red")
    os.symlink("Projects/aurora", os.path.join(D, "aurora-link"))


# ── the app ──
app_proc = None


def start(*args, fresh=True, window=True):
    """Launch Kestrel and wait for its window; returns the accessible application."""
    global app_proc
    stop()
    if fresh:
        shutil.rmtree(CONF, ignore_errors=True)
    app_proc = subprocess.Popen([os.path.join(BIN, "kestrel"), *args], cwd=HOME,
                                stdout=open(os.path.join(OUT, "app.log"), "a"), stderr=subprocess.STDOUT)
    if not window:  # D-Bus activated: no window, and not on the accessibility bus until it has one
        time.sleep(2)
        return None
    a = wait(lambda: find_app(), "the app on the accessibility bus")
    wait(lambda: any(n.get_role_name() == "frame" for n in kids(a)), "a window")
    time.sleep(0.8)
    return a


def stop():
    global app_proc
    if app_proc and app_proc.poll() is None:
        app_proc.kill()
        try:
            app_proc.wait(5)
        except subprocess.TimeoutExpired:
            app_proc.kill()
    app_proc = None


def find_app():
    desk = Atspi.get_desktop(0)
    for i in range(desk.get_child_count()):
        a = desk.get_child_at_index(i)
        try:
            if a and a.get_name() == "kestrel" and a.get_process_id() == app_proc.pid:
                return a
        except Exception:
            pass


def kids(n):
    out = []
    for i in range(n.get_child_count()):
        c = n.get_child_at_index(i)
        if c:
            out.append(c)
    return out


def walk(n, depth=0):
    yield n
    if depth > 60:
        return
    try:
        cs = kids(n)
    except Exception:
        return
    for c in cs:
        yield from walk(c, depth + 1)


def visible(n):
    try:
        return n.get_state_set().contains(SHOWING)
    except Exception:
        return False


def app():
    return find_app()


def all_(role=None, name=None, root=None, contains=None):
    out = []
    for n in walk(root or app()):
        try:
            if role and n.get_role_name() != role:
                continue
            nm = n.get_name() or ""
            if name is not None and nm != name:
                continue
            if contains is not None and contains not in nm:
                continue
            if visible(n):
                out.append(n)
        except Exception:
            pass
    return out


def one(role=None, name=None, contains=None, root=None, timeout=5):
    return wait(lambda: (all_(role, name, root, contains) or [None])[0], f"{role} {name or contains or ''}".strip(), timeout)


def wait(pred, what, timeout=6):
    end = time.time() + timeout
    while True:
        try:
            v = pred()
        except Exception:
            v = None
        if v:
            return v
        if time.time() > end:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(0.15)


def names():
    """File names in the visible file views, in order. Details: first cell of each row; grid: each
    cell of the table; columns: unnamed list items (sidebar rows have names, so they're left out)."""
    out = []
    for n in all_():
        r = n.get_role_name()
        if r == "table row":
            cells = [c for c in kids(n) if c.get_role_name() == "table cell"]
            if cells:
                out.append(cells[0].get_name())
        elif r == "table cell" and n.get_parent().get_role_name() == "table":
            out.append(n.get_name())
        elif r == "list item" and not n.get_name():
            label = next((x for x in walk(n) if x.get_role_name() == "label" and x.get_name()), None)
            if label:
                out.append(label.get_name())
    return out


def row(name):
    return one("table cell", name)


def status():
    for n in all_("label"):
        t = n.get_name() or ""
        if " item" in t or " result" in t:
            return t
    return ""


def dump(n, out, depth=0):
    """The visible accessibility tree, for working out why a test failed."""
    if not visible(n) and depth:
        return
    out.write("  " * depth + f"{n.get_role_name()}: {n.get_name()!r}\n")
    for c in kids(n):
        dump(c, out, depth + 1)


# ── input ──
SYM = {".": "period", "/": "slash", "_": "underscore", "-": "minus", " ": "space", ":": "colon", "*": "asterisk",
       "|": "bar", "!": "exclam", '"': "quotedbl", "<": "less", ">": "greater", "~": "asciitilde", "=": "equal",
       "\n": "Return", "'": "apostrophe", ",": "comma", "$": "dollar", "&": "ampersand", "(": "parenleft", ")": "parenright"}


def _code(name):
    ks = XK.string_to_keysym(name)
    kc = dpy.keysym_to_keycode(ks)
    shift = kc and dpy.keycode_to_keysym(kc, 0) != ks
    return kc, shift


def key(*names, pause=0.5):
    codes = []
    for n in names:
        kc, shift = _code(n)
        if not kc:
            raise AssertionError(f"no key for {n}")
        if shift and "Shift_L" not in names:
            codes.append(_code("Shift_L")[0])
        codes.append(kc)
    for c in codes:
        xtest.fake_input(dpy, X.KeyPress, c)
    for c in reversed(codes):
        xtest.fake_input(dpy, X.KeyRelease, c)
    dpy.sync()
    time.sleep(pause)


def typ(text):
    for ch in text:
        key(SYM.get(ch, ch), pause=0.02)
    time.sleep(0.4)


def point(acc):
    e = Atspi.Component.get_extents(acc, Atspi.CoordType.WINDOW)
    return e.x + min(24, max(1, e.width // 2)), e.y + e.height // 2


def click(acc, button=1, double=False, mods=()):
    x, y = point(acc)
    xtest.fake_input(dpy, X.MotionNotify, x=x, y=y)
    for m in mods:
        xtest.fake_input(dpy, X.KeyPress, _code(m)[0])
    for _ in range(2 if double else 1):
        xtest.fake_input(dpy, X.ButtonPress, button)
        xtest.fake_input(dpy, X.ButtonRelease, button)
    for m in mods:
        xtest.fake_input(dpy, X.KeyRelease, _code(m)[0])
    dpy.sync()
    time.sleep(0.6)


def drag(src, dst):
    (x1, y1), (x2, y2) = point(src), point(dst)
    x1 += 16  # on the icon, past the cell's padding
    xtest.fake_input(dpy, X.MotionNotify, x=x1, y=y1)
    xtest.fake_input(dpy, X.ButtonPress, 1)
    dpy.sync()
    time.sleep(0.3)
    for i in range(1, 41):  # GTK starts a drag only after the pointer moves a few pixels
        xtest.fake_input(dpy, X.MotionNotify, x=x1 + (x2 - x1) * i // 40, y=y1 + (y2 - y1) * i // 40)
        dpy.sync()
        time.sleep(0.04)
    for dx in (2, -2, 1, 0):  # hover over the target a moment, as a hand does
        xtest.fake_input(dpy, X.MotionNotify, x=x2 + dx, y=y2)
        dpy.sync()
        time.sleep(0.15)
    xtest.fake_input(dpy, X.ButtonRelease, 1)
    dpy.sync()
    time.sleep(0.8)


def press_button(name, contains=None):
    """Buttons by their accessible "click" action: no coordinates, so dialogs and popovers work."""
    b = one("button", name, contains=contains)
    if not activate(b):
        click(b)
    time.sleep(0.5)


def activate(acc):
    try:
        if Atspi.Action.get_n_actions(acc) > 0:
            return Atspi.Action.do_action(acc, 0)
    except Exception:
        pass
    return False


def set_text(acc, text):
    Atspi.EditableText.set_text_contents(acc, text)
    time.sleep(0.3)


def text_of(acc):
    return Atspi.Text.get_text(acc, 0, -1)


def select(*ns):
    click(row(ns[0]))
    for n in ns[1:]:
        click(row(n), mods=("Control_L",))


def label_of(n):
    """A widget's name, or the text of what it's labelled by (GTK menu items name themselves that way)."""
    nm = n.get_name() or ""
    if nm:
        return nm
    for r in n.get_relation_set() or []:
        if r.get_relation_type() == Atspi.RelationType.LABELLED_BY:
            for i in range(r.get_n_targets()):
                try:
                    return Atspi.Text.get_text(r.get_target(i), 0, -1)
                except Exception:
                    pass
    return ""


def pick(item):
    """Choose an item in the open menu."""
    it = wait(lambda: [n for n in all_("menu item") if label_of(n) == item][0], f"menu item {item}")
    if not activate(it):
        click(it)
    time.sleep(0.6)


def menu(item):
    """Right-click menu of the selection: open it and pick an item."""
    key("Shift_L", "F10")
    pick(item)


def screenshot(path):
    root = dpy.screen().root
    g = root.get_geometry()
    raw = root.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xFFFFFFFF).data
    rows = b"".join(b"\0" + bytes(v for px in range(g.width) for v in raw[(y * g.width + px) * 4:(y * g.width + px) * 4 + 3][::-1])
                    for y in range(g.height))
    chunk = lambda t, b: struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b))
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", g.width, g.height, 8, 2, 0, 0, 0))
                           + chunk(b"IDAT", zlib.compress(rows, 1)) + chunk(b"IEND", b""))


def exists(*p):
    return os.path.lexists(os.path.join(D, *p))


def eventually(pred, what, timeout=6):
    wait(pred, what, timeout)


# ── the private search service (static: started on the demo folder as it is) ──
ixd = None


def index():
    global ixd
    if ixd and ixd.poll() is None:
        ixd.kill()
        ixd.wait()
    sock = os.environ["KESTREL_SOCK"]
    if os.path.exists(sock):
        os.unlink(sock)
    log = open(os.path.join(OUT, "indexd.log"), "a")
    ixd = subprocess.Popen([os.path.join(BIN, "kestrel-indexd"), "--no-watch"], stderr=log,
                           env=dict(os.environ, KESTREL_ROOT=D, KESTREL_STATE=os.path.join(RUN, "index.bin")))
    wait(lambda: subprocess.run([os.path.join(BIN, "ks"), "-n", "1", "notes.md"], capture_output=True).stdout, "the search service")


def ks(q):
    r = subprocess.run([os.path.join(BIN, "ks"), q], capture_output=True, text=True)
    return [os.path.relpath(l, D) for l in r.stdout.splitlines()]


# ── tests ──
TESTS = []


def test(name):
    def reg(f):
        TESTS.append((name, f))
        return f
    return reg


# navigation
@test("navigate: double-click opens a folder, Back / Forward / Up")
def _():
    start(D)
    click(row("Projects"), double=True)
    eventually(lambda: names() == ["aurora"], "Projects' contents")
    key("Alt_L", "Left"); eventually(lambda: "notes.md" in names(), "back in Demo")
    key("Alt_L", "Right"); eventually(lambda: names() == ["aurora"], "forward to Projects")
    key("Alt_L", "Up"); eventually(lambda: "notes.md" in names(), "up to Demo")


@test("navigate: the mouse's back and forward buttons")
def _():
    start(D)
    click(row("Projects"), double=True); eventually(lambda: names() == ["aurora"], "Projects")
    click(row("aurora"), button=8); eventually(lambda: "notes.md" in names(), "back in Demo")
    click(row("notes.md"), button=9); eventually(lambda: names() == ["aurora"], "forward to Projects")


@test("navigate: Enter opens the selected folder, Backspace goes up")
def _():
    start(D)
    select("Projects"); key("Return")
    eventually(lambda: names() == ["aurora"], "Projects' contents")
    key("BackSpace"); eventually(lambda: "notes.md" in names(), "up to Demo")


@test("navigate: typed location (Ctrl+L)")
def _():
    start(D)
    key("Control_L", "l")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "location entry")
    set_text(e, os.path.join(D, "Projects/aurora")); key("Return")
    eventually(lambda: names() == ["src"], "aurora's contents")


@test("navigate: breadcrumb click")
def _():
    start(os.path.join(D, "Projects/aurora"))
    press_button("Demo")
    eventually(lambda: "notes.md" in names(), "Demo")


@test("navigate: sidebar Home")
def _():
    start(os.path.join(D, "Projects"))
    click(one("list item", "Home"))
    eventually(lambda: "Demo" in names(), "the home folder")


@test("tabs: Ctrl+T opens, Ctrl+W closes")
def _():
    start(D)
    n = len(all_("page tab"))
    key("Control_L", "t"); eventually(lambda: len(all_("page tab")) == n + 1, "a second tab")
    key("Control_L", "w"); eventually(lambda: len(all_("page tab")) == n, "back to one tab")


@test("windows: Ctrl+N opens a second window")
def _():
    start(D)
    key("Control_L", "n")
    eventually(lambda: len([f for f in kids(app()) if f.get_role_name() == "frame"]) == 2, "two windows")


@test("session: tabs come back after a restart")
def _():
    start(D)
    key("Control_L", "t"); time.sleep(0.5)
    key("Control_L", "l")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "location entry")
    set_text(e, os.path.join(D, "Projects")); key("Return")
    eventually(lambda: names() == ["aurora"], "Projects")
    press_button("Close")  # closing the window saves the session
    eventually(lambda: os.path.exists(os.path.join(CONF, "session.json")), "session.json")
    start(fresh=False)
    eventually(lambda: any((t.get_name() or "") == "Projects" for t in all_("page tab")), "the Projects tab restored")


@test("view: hidden files (Ctrl+H)")
def _():
    start(D)
    assert ".hidden-file" not in names()
    key("Control_L", "h"); eventually(lambda: ".hidden-file" in names(), "hidden file shown")
    key("Control_L", "h"); eventually(lambda: ".hidden-file" not in names(), "hidden file hidden again")


@test("view: details / grid / columns (Ctrl+1/2/3)")
def _():
    start(D)
    key("Control_L", "2"); eventually(lambda: not all_("table row") and "notes.md" in names(), "grid view")
    key("Control_L", "3"); eventually(lambda: not all_("table row") and "Projects" in names() and all_("list item", ""), "column view")
    key("Control_L", "1"); eventually(lambda: "notes.md" in names() and all_("table row"), "details view")


@test("view: sidebar toggles (F9)")
def _():
    start(D)
    one("list item", "Home")
    key("F9"); eventually(lambda: not all_("list item", "Home"), "sidebar hidden")
    key("F9"); eventually(lambda: all_("list item", "Home"), "sidebar back")


@test("sidebar: File System shows free space")
def _():
    start(D)
    assert one("list item", contains="free of"), "no free-space row"


@test("bookmarks: Ctrl+D adds the folder, shared with GNOME Files")
def _():
    start(os.path.join(D, "Projects"))
    key("Control_L", "d")
    bm = os.path.join(HOME, ".config/gtk-3.0/bookmarks")
    eventually(lambda: os.path.exists(bm) and "Projects" in open(bm).read(), "the bookmarks file")
    eventually(lambda: all_("list item", contains="Projects"), "the bookmark in the sidebar")


@test("dual pane: F3 opens it, F7 copies, F8 moves to the other pane")
def _():
    start(D)
    key("F3"); eventually(lambda: len(all_("tree table")) >= 2, "two panes")
    key("F4")  # the right pane: point it at Projects
    key("Control_L", "l")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "location entry")
    set_text(e, os.path.join(D, "Projects")); key("Return")
    eventually(lambda: "aurora" in names() and "notes.md" in names(), "Demo left, Projects right")
    key("F4")
    select("IMG_0001.txt"); key("F7")
    eventually(lambda: exists("Projects/IMG_0001.txt") and exists("IMG_0001.txt"), "copied to Projects")
    select("IMG_0002.txt"); key("F8")
    eventually(lambda: exists("Projects/IMG_0002.txt") and not exists("IMG_0002.txt"), "moved to Projects")


# file operations
@test("files: copy and paste (Ctrl+C / Ctrl+V)")
def _():
    start(D)
    select("IMG_0001.txt"); key("Control_L", "c")
    click(row("Projects"), double=True); eventually(lambda: names() == ["aurora"], "Projects")
    key("Control_L", "v")
    eventually(lambda: exists("Projects/IMG_0001.txt") and exists("IMG_0001.txt"), "the copy")
    eventually(lambda: "IMG_0001.txt" in names(), "the copy listed")


@test("files: cut and paste moves")
def _():
    start(D)
    select("IMG_0001.txt", "IMG_0003.txt"); key("Control_L", "x")
    click(row("Projects"), double=True); eventually(lambda: names() == ["aurora"], "Projects")
    key("Control_L", "v")
    eventually(lambda: exists("Projects/IMG_0001.txt") and exists("Projects/IMG_0003.txt") and not exists("IMG_0001.txt"), "moved")


@test("files: paste into the same folder makes a numbered copy")
def _():
    start(D)
    select("notes.md"); key("Control_L", "c"); key("Control_L", "v")
    eventually(lambda: exists("notes (2).md") and exists("notes.md"), "notes (2).md")


@test("files: a name clash asks; Keep Both keeps both")
def _():
    open(os.path.join(D, "Projects/notes.md"), "w").write("old\n")
    start(D)
    select("notes.md"); key("Control_L", "c")
    click(row("Projects"), double=True); eventually(lambda: "aurora" in names(), "Projects")
    key("Control_L", "v"); one("alert", contains="already exists")
    press_button("Keep Both")
    eventually(lambda: exists("Projects/notes (2).md") and open(os.path.join(D, "Projects/notes.md")).read() == "old\n", "both kept")


@test("files: Replace puts the old one in the trash, Skip leaves it")
def _():
    open(os.path.join(D, "Projects/notes.md"), "w").write("old\n")
    start(D)
    select("notes.md"); key("Control_L", "c")
    click(row("Projects"), double=True); eventually(lambda: "aurora" in names(), "Projects")
    key("Control_L", "v"); one("alert", contains="already exists"); press_button("Skip")
    time.sleep(0.8)
    assert open(os.path.join(D, "Projects/notes.md")).read() == "old\n", "Skip changed the file"
    click(row("aurora")); key("Control_L", "v"); one("alert", contains="already exists"); press_button("Replace")
    eventually(lambda: "Aurora" in open(os.path.join(D, "Projects/notes.md")).read(), "the replacement")
    eventually(lambda: os.path.exists(os.path.join(TRASH, "notes.md")), "the old one in the trash")


@test("files: rename (F2) and undo (Ctrl+Z)")
def _():
    start(D)
    select("IMG_0001.txt"); key("F2")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if text_of(x) == "IMG_0001.txt"][0], "the rename field")
    # typed, with a space: Space opens the preview in the file list but must still type here
    key("Control_L", "a"); typ("holiday photo.txt"); key("Return")
    eventually(lambda: exists("holiday photo.txt") and not exists("IMG_0001.txt"), "renamed on disk")
    key("Control_L", "z")
    eventually(lambda: exists("IMG_0001.txt") and not exists("holiday photo.txt"), "undone")


@test("files: new folder (Ctrl+Shift+N)")
def _():
    start(D)
    key("Control_L", "Shift_L", "n")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if text_of(x) == "New Folder"][0], "the name field")
    set_text(e, "Fresh"); key("Return")
    eventually(lambda: os.path.isdir(os.path.join(D, "Fresh")), "the folder on disk")
    eventually(lambda: "Fresh" in names(), "the folder listed")


def palette(cmd):
    key("Control_L", "Shift_L", "p")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "the palette field")
    set_text(e, cmd); key("Return")


@test("files: New > Empty File from the command bar")
def _():
    start(D)
    press_button("New"); pick("Empty File…")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if text_of(x) == "New Document.txt"][0], "the name field")
    set_text(e, "fresh.txt"); key("Return")
    eventually(lambda: os.path.isfile(os.path.join(D, "fresh.txt")), "the file on disk")


@test("files: Paste as Link makes a symlink")
def _():
    start(D)
    select("notes.md"); key("Control_L", "c")
    palette("paste as link")
    p = os.path.join(D, "notes (2).md")
    eventually(lambda: os.path.islink(p) and os.readlink(p) == os.path.join(D, "notes.md"), "a link to notes.md")


@test("files: select all, invert, none (Ctrl+A, Ctrl+Shift+I, Ctrl+Shift+A)")
def _():
    start(D)
    select("notes.md")
    key("Control_L", "a"); eventually(lambda: "all" in status() or f"{len(names())} selected" in status(), "all selected")
    select("notes.md"); key("Control_L", "Shift_L", "i")
    eventually(lambda: f"{len(names()) - 1} selected" in status(), "all but one selected")
    key("Control_L", "Shift_L", "a"); eventually(lambda: "selected" not in status(), "none selected")


@test("files: Delete moves to the trash, Ctrl+Z brings it back")
def _():
    start(D)
    select("IMG_0003.txt"); key("Delete")
    eventually(lambda: not exists("IMG_0003.txt") and os.path.exists(os.path.join(TRASH, "IMG_0003.txt")), "in the trash")
    key("Control_L", "z")
    eventually(lambda: exists("IMG_0003.txt"), "restored by undo")


@test("files: Trash view, Restore")
def _():
    start(D)
    select("IMG_0003.txt"); key("Delete")
    eventually(lambda: os.path.exists(os.path.join(TRASH, "IMG_0003.txt")), "in the trash")
    click(one("list item", "Trash"))
    eventually(lambda: "IMG_0003.txt" in names(), "listed in Trash")
    select("IMG_0003.txt"); menu("Restore")
    eventually(lambda: exists("IMG_0003.txt"), "restored to Demo")


@test("files: Shift+Delete deletes for good after asking")
def _():
    start(D)
    select("IMG_0003.txt"); key("Shift_L", "Delete")
    press_button("Delete")
    eventually(lambda: not exists("IMG_0003.txt"), "gone")
    assert not os.path.exists(os.path.join(TRASH, "IMG_0003.txt")), "it went to the trash instead"


@test("files: Empty Trash")
def _():
    start(D)
    select("IMG_0003.txt"); key("Delete")
    eventually(lambda: os.path.exists(os.path.join(TRASH, "IMG_0003.txt")), "in the trash")
    click(one("list item", "Trash")); eventually(lambda: "IMG_0003.txt" in names(), "listed in Trash")
    # background menu: right-click empty space below the rows
    # the background menu: right-click the empty space under the rows
    xtest.fake_input(dpy, X.MotionNotify, x=700, y=550)
    xtest.fake_input(dpy, X.ButtonPress, 3); xtest.fake_input(dpy, X.ButtonRelease, 3); dpy.sync(); time.sleep(0.8)
    pick("Empty Trash…")
    press_button("Empty Trash")
    eventually(lambda: not os.listdir(TRASH), "an empty trash")


class Skip(Exception):
    pass


@test("files: drag a file onto a folder moves it")
def _():
    # ponytail: a synthetic XTest drag starts (no rubber band since 0.9.17) but GTK's X11 drop never
    # completes on a bare Xvfb; run with FT_DND=1 on a real desktop session to check the drop
    if not os.environ.get("FT_DND"):
        raise Skip("drop needs a real desktop session (FT_DND=1)")
    start(D)
    drag(row("IMG_0001.txt"), row("Projects"))
    eventually(lambda: exists("Projects/IMG_0001.txt") and not exists("IMG_0001.txt"), "moved into Projects")


@test("files: Copy Path (Ctrl+Shift+C)")
def _():
    start(D)
    select("notes.md"); key("Control_L", "Shift_L", "c")
    key("Control_L", "l")  # paste it into the location bar, a plain text field
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "location entry")
    key("Control_L", "a"); key("Control_L", "v")
    eventually(lambda: text_of(e) == os.path.join(D, "notes.md"), "the copied path")


@test("batch rename: find and replace across three files")
def _():
    start(D)
    select("IMG_0001.txt", "IMG_0002.txt", "IMG_0003.txt"); key("F2")
    find = wait(lambda: [x for x in all_("text") if (x.get_name() or "") == "Find"][0], "the Find field")
    set_text(find, "IMG_")
    repl = wait(lambda: [x for x in all_("text") if (x.get_name() or "") == "Replace with"][0], "the Replace field")
    set_text(repl, "Photo ")
    eventually(lambda: all_(contains="Photo 0001.txt"), "the live preview")
    press_button("Rename")
    eventually(lambda: all(exists(f"Photo 000{i}.txt") for i in (1, 2, 3)), "renamed on disk")


@test("tags: a tag set in the preview pane lands on the file")
def _():
    start(D)
    select("notes.md"); key("Alt_L", "p")
    press_button("Tag Blue")
    eventually(lambda: b"blue" in os.getxattr(os.path.join(D, "notes.md"), "user.chimera.tags"), "the tag xattr")


# search
@test("search: typing finds files; Esc goes back to the folder")
def _():
    index(); start(D)
    e = one("entry", "Search files"); click(e); typ("IMG")
    eventually(lambda: sorted(names()) == ["IMG_0001.txt", "IMG_0002.txt", "IMG_0003.txt"], "three results")
    eventually(lambda: "3 results" in status(), "the result count")
    key("Escape"); eventually(lambda: "notes.md" in names() and "Projects" in names(), "the folder again")


@test("search: type words rank files of that type first (pdf, excel)")
def _():
    index(); start(D)
    e = one("entry", "Search files"); click(e); typ("pdf")
    eventually(lambda: names()[:1] == ["report.pdf"], "report.pdf first")
    set_text(e, "budget excel")
    eventually(lambda: names()[:1] == ["budget.xlsx"], "budget.xlsx for 'budget excel'")


@test("search: Ctrl+F searches only this folder")
def _():
    index(); start(os.path.join(D, "Projects"))
    key("Control_L", "f"); typ("main")
    eventually(lambda: names() == ["main.rs"], "main.rs")
    key("Escape"); key("Escape")
    key("Control_L", "f"); typ("IMG")
    eventually(lambda: all_(None, "No results"), "no IMG files under Projects")


@test("search: tag search from the sidebar dots")
def _():
    index(); start(D)
    press_button("Files tagged red")
    eventually(lambda: names() == ["IMG_0002.txt"], "the red-tagged file")


@test("search: content search finds text inside files")
def _():
    index(); start(D)
    e = one("entry", "Search files"); click(e); typ("content:weather")
    eventually(lambda: names() == ["notes.md"], "notes.md")


@test("search: Ctrl+D saves the search; it appears under Saved Searches")
def _():
    index(); start(D)
    e = one("entry", "Search files"); click(e); typ("IMG")
    eventually(lambda: len(names()) == 3, "results")
    key("Control_L", "d")
    eventually(lambda: all_("list item", contains="IMG"), "the saved search in the sidebar")


@test("search: no results says so")
def _():
    index(); start(D)
    e = one("entry", "Search files"); click(e); typ("zzqqxx")
    one(None, "No results")


@test("search: the syntax cheat sheet opens")
def _():
    start(D)
    t = one("toggle button", "Search syntax")
    if not activate(t):
        click(t)
    one(None, contains="wildcard")


@test("search: the query language through ks")
def _():
    index()
    cases = {
        "*.md": {"notes.md"},
        "notes.md|report.pdf": {"notes.md", "report.pdf"},
        "IMG !0002": {"IMG_0001.txt", "IMG_0003.txt"},
        "ext:rs": {"Projects/aurora/src/main.rs"},
        "folder:aurora": {"Projects/aurora"},
        "regex:^IMG_000[12]": {"IMG_0001.txt", "IMG_0002.txt"},
        "size:<5 IMG": set(),
        "tag:red": {"IMG_0002.txt"},
        "content:println": {"Projects/aurora/src/main.rs"},
        "path:src main": {"Projects/aurora/src/main.rs"},
    }
    bad = [f"{q!r} gave {sorted(set(ks(q)))}, want {sorted(want)}" for q, want in cases.items() if set(ks(q)) != want]
    assert not bad, "; ".join(bad)


# preview, properties
@test("preview: Space opens the pane; Markdown is rendered")
def _():
    start(D)
    select("notes.md"); key("space")
    one("label", "Preview")
    eventually(lambda: all_(contains="weather station"), "the rendered text")


@test("preview: a BMP image shows (not just its icon)")
def _():
    w, h = 40, 30
    row_bytes = (w * 3 + 3) & ~3
    px = b"".join(bytes([x * 6 % 256, y * 8 % 256, 160]) * 1 for y in range(h) for x in range(w))
    rows = b"".join(px[y * w * 3:(y + 1) * w * 3] + b"\0" * (row_bytes - w * 3) for y in range(h))
    hdr = struct.pack("<2sIHHI", b"BM", 54 + len(rows), 0, 0, 54) + struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(rows), 2835, 2835, 0, 0)
    open(os.path.join(D, "screen.bmp"), "wb").write(hdr + rows)
    start(D)
    select("screen.bmp"); key("space")
    one("label", "Preview")
    time.sleep(1.5)
    assert not all_(contains="No preview for this image format"), "the BMP didn't decode"
    pics = [n for n in all_("image") if (Atspi.Component.get_extents(n, Atspi.CoordType.WINDOW).height or 0) > 100]
    assert pics, "no picture in the preview"


def make_song(path, freq):
    """A 6-second song (tagged MP3, or WAV by extension; needs gst-launch-1.0), or None."""
    enc = ["wavenc"] if path.endswith(".wav") else ["lamemp3enc", "!", "id3v2mux"]
    r = subprocess.run(["gst-launch-1.0", "-q", "audiotestsrc", "num-buffers=260", f"freq={freq}", "!", "audioconvert", "!",
                        *enc, "!", "filesink", f"location={path}"], capture_output=True)
    return path if r.returncode == 0 and os.path.getsize(path) > 0 else None


@test("music: MP3 and WAV play, next / previous walk the folder, repeat cycles, a song's end moves on")
def _():
    music = os.path.join(D, "Music")
    os.makedirs(music)
    if not all(make_song(os.path.join(music, n), f) for n, f in (("a song.mp3", 440), ("b song.wav", 550), ("c song.mp3", 660))):
        raise Skip("gst-launch-1.0 / lamemp3enc not available to make test songs")
    extra = os.environ.get("FT_SONG")  # FT_SONG=some.mp3: also a real file (one once crashed GStreamer)
    if extra:
        shutil.copy(extra, os.path.join(music, "0 real.mp3"))
    order = sorted(os.listdir(music))
    start(music)
    select(order[0]); key("space")
    press_button("Play")
    eventually(lambda: all_("button", "Pause"), "playing (the button turns to Pause)")
    eventually(lambda: any(" / " in (n.get_name() or "") for n in all_("label")), "the elapsed / total time")
    time.sleep(1)
    assert app_proc.poll() is None, "Kestrel crashed while playing"
    press_button("Next song in this folder")
    eventually(lambda: all_("label", order[1]), "the next song")
    press_button("Previous song in this folder")
    eventually(lambda: all_("label", order[0]), "back to the first song")
    for want in ("Repeat: this folder", "Repeat: this song", "Repeat: off"):
        press_button(None, contains="Repeat:")
        eventually(lambda: all_("button", want), want)
    if not extra:  # the first song ends and the next (the WAV) starts by itself and plays
        eventually(lambda: all_("label", order[1]), "the next song after the first ended", timeout=10)
        eventually(lambda: all_("button", "Pause") and any("0:0" in (n.get_name() or "") and " / " in (n.get_name() or "") for n in all_("label")), "the WAV playing")
        assert not all_(contains="Can't play this"), "the WAV didn't play"
    assert app_proc.poll() is None, "Kestrel crashed"


@test("preview: a folder shows its size from the index")
def _():
    index(); start(D)
    select("Projects"); key("space")
    eventually(lambda: all_(contains="items inside"), "the folder total")


@test("properties: Alt+Return; checksums calculate and compare")
def _():
    start(D)
    select("notes.md"); key("Alt_L", "Return")
    one(None, "Properties")
    press_button("Calculate")
    sha = hashlib.sha256(open(os.path.join(D, "notes.md"), "rb").read()).hexdigest()
    eventually(lambda: all_(contains=sha), "the SHA-256")
    cmp = wait(lambda: [x for x in all_("text") if (x.get_name() or "") == "Compare with"][0], "the Compare field")
    set_text(cmp, sha)
    eventually(lambda: all_(contains="Matches") or any("Matches" in (n.get_description() or "") for n in all_()), "Matches")


@test("properties: 'Allow running as a program' sets the execute bit")
def _():
    p = os.path.join(D, "notes.md")
    assert not os.stat(p).st_mode & 0o100
    start(D)
    select("notes.md"); key("Alt_L", "Return")
    one(None, "Properties")
    # the row and the switch inside it share the name; the inner one toggles
    sw = wait(lambda: all_("switch", "Allow running as a program")[-1], "the switch")
    if not activate(sw):
        click(sw)
    eventually(lambda: os.stat(p).st_mode & 0o100, "the execute bit on disk")


@test("properties: Calculate totals a folder the index doesn't cover (a network share)")
def _():
    # FT_NETDIR=/run/user/…/gvfs/smb-share:…/Folder runs it on a real network folder too
    target = os.environ.get("FT_NETDIR") or os.path.join(D, "Projects")
    want_n = sum(len(ds) + len(fs) for _, ds, fs in os.walk(target))
    if ixd and ixd.poll() is None:  # an earlier test's index would answer instead of counting
        ixd.kill()
        ixd.wait()
    start(os.path.dirname(target))  # no index running: the size has to be counted
    select(os.path.basename(target)); key("Alt_L", "Return")
    one(None, "Properties")
    press_button("Calculate")
    eventually(lambda: all_(contains=f"for {want_n} items"), f"'… for {want_n} items'", timeout=120)


@test("network: the page lists previous servers (shared with GNOME Files) and can forget one")
def _():
    gtk4 = os.path.join(HOME, ".config/gtk-4.0")
    os.makedirs(gtk4, exist_ok=True)
    servers = os.path.join(gtk4, "servers")
    open(servers, "w").write('<?xml version="1.0"?>\n<xbel version="1.0">\n  <bookmark href="sftp://tester@192.0.2.7/srv" added="2026-01-01T00:00:00Z" visited="2026-01-01T00:00:00Z">\n  </bookmark>\n</xbel>\n')
    start(D)
    click(one("list item", "Network"))
    one(None, "Previous"); one(None, "Connect to Server")
    one(None, contains="tester@192.0.2.7/srv")
    press_button("Forget this server")
    eventually(lambda: "192.0.2.7" not in open(servers).read(), "the server removed from GTK's servers file")


# palette, preferences, terminal, scripts, git, open, FileManager1
@test("palette: Ctrl+Shift+P runs a command by name")
def _():
    start(D)
    n = len(all_("page tab"))
    key("Control_L", "Shift_L", "p")
    e = wait(lambda: [x for x in all_("text") + all_("entry") if (x.get_name() or "") != "Search files"][0], "the palette field")
    set_text(e, "new tab"); key("Return")
    eventually(lambda: len(all_("page tab")) == n + 1, "a new tab")


@test("preferences: Show hidden files is saved and applied")
def _():
    start(D)
    key("Control_L", "comma")
    sw = wait(lambda: [s for s in all_("switch") if "hidden" in (s.get_name() or "").lower()][0] if any("hidden" in (s.get_name() or "").lower() for s in all_("switch")) else [r for r in all_(contains="Show hidden files")][0], "the hidden-files switch")
    click(sw)
    eventually(lambda: '"show_hidden": true' in open(os.path.join(CONF, "settings.json")).read().replace('"show_hidden":true', '"show_hidden": true'), "settings.json")


@test("terminal: Ctrl+` opens a shell in this folder; it follows navigation")
def _():
    start(D)
    key("Control_L", "grave")
    term = one("terminal", timeout=8)
    time.sleep(1.5)
    click(term); typ("touch made-here\n")
    eventually(lambda: exists("made-here"), "a file made by the shell in Demo")
    click(row("Projects"), double=True); eventually(lambda: names() == ["aurora"], "Projects")
    time.sleep(1.5)
    click(term); typ("touch made-there\n")
    eventually(lambda: exists("Projects/made-there"), "the shell followed to Projects")


@test("scripts: a Nautilus script runs with the selection")
def _():
    sd = os.path.join(HOME, ".local/share/nautilus/scripts")
    os.makedirs(sd, exist_ok=True)
    s = os.path.join(sd, "Record selection")
    open(s, "w").write('#!/bin/sh\nprintf "%s" "$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS" > "$HOME/script-ran"\n')
    os.chmod(s, 0o755)
    out = os.path.join(HOME, "script-ran")
    if os.path.exists(out):
        os.unlink(out)
    start(D)
    select("notes.md"); key("Shift_L", "F10")
    pick("Scripts"); pick("Record selection")
    eventually(lambda: os.path.exists(out) and "notes.md" in open(out).read(), "the script's output")


@test("git: a repository shows its branch and changed files")
def _():
    g = os.path.join(D, "repo")
    os.makedirs(g)
    run = lambda *a: subprocess.run(["git", "-C", g, *a], capture_output=True, check=True)
    run("init", "-q", "-b", "trunk"); open(os.path.join(g, "a.txt"), "w").write("1\n")
    run("add", "."); run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x")
    open(os.path.join(g, "a.txt"), "w").write("2\n")
    start(g)
    eventually(lambda: all_(contains="trunk"), "the branch name")
    eventually(lambda: any("odified" in (n.get_name() or "") or (n.get_name() or "") == "M" for n in all_("table cell")), "a.txt marked modified")


@test("open: double-clicking a file opens it in its default app")
def _():
    apps = os.path.join(HOME, ".local/share/applications")
    os.makedirs(apps, exist_ok=True)
    open(os.path.join(apps, "ft-open.desktop"), "w").write(
        "[Desktop Entry]\nType=Application\nName=FT Open\nExec=sh -c 'echo %f > \"$HOME/opened\"'\nMimeType=text/plain;\n")
    open(os.path.join(HOME, ".config/mimeapps.list"), "w").write("[Default Applications]\ntext/plain=ft-open.desktop\n")
    out = os.path.join(HOME, "opened")
    if os.path.exists(out):
        os.unlink(out)
    start(D)
    click(row("IMG_0001.txt"), double=True)
    eventually(lambda: os.path.exists(out) and "IMG_0001.txt" in open(out).read(), "the default app launched")


@test("default file manager: FileManager1.ShowItems opens and selects")
def _():
    start("--filemanager1", window=False)
    uri = "file://" + os.path.join(D, "report.pdf")
    subprocess.run(["gdbus", "call", "--session", "--dest", "org.freedesktop.FileManager1", "--object-path",
                    "/org/freedesktop/FileManager1", "--method", "org.freedesktop.FileManager1.ShowItems", f"['{uri}']", ""],
                   capture_output=True, timeout=10)
    eventually(lambda: "notes.md" in names(), "a window on Demo")
    eventually(lambda: "1 selected" in status(), "report.pdf selected")


# ── runner ──
def main():
    only = sys.argv[1] if len(sys.argv) > 1 else ""
    ran = failed = 0
    for name, f in TESTS:
        if only and only not in name:
            continue
        ran += 1
        demo()
        t = time.time()
        try:
            f()
            print(f"ok   {name} ({time.time() - t:.1f}s)", flush=True)
        except Skip as e:
            ran -= 1
            print(f"skip {name}: {e}", flush=True)
        except Exception as e:
            failed += 1
            slug = "".join(c if c.isalnum() else "-" for c in name)[:60]
            try:
                screenshot(os.path.join(OUT, slug + ".png"))
                with open(os.path.join(OUT, slug + ".tree.txt"), "w") as t:
                    dump(app(), t)
            except Exception:
                pass
            msg = str(e) if isinstance(e, AssertionError) else traceback.format_exc(limit=2).strip().splitlines()[-1]
            print(f"FAIL {name}: {msg}  [screenshot: target/feature-test/{slug}.png]", flush=True)
        finally:
            stop()
    if ixd and ixd.poll() is None:
        ixd.kill()
    # GVfs's network browsing starts wsdd in this session; it outlives dbus-run-session otherwise
    subprocess.run(["pkill", "-f", f"wsdd .*--listen {RUN}/"], capture_output=True)
    print(f"{ran - failed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


main()
