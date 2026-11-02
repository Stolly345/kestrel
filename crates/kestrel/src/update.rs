//! Once-a-day check for a newer GitHub release; shows a toast with a link to it.
//! ponytail: reads the tag from the /releases/latest redirect via curl, so no HTTP crate or JSON.

use gtk::{gio, glib};

const LATEST: &str = "https://github.com/Stolly345/kestrel/releases/latest";
const DAY: u64 = 24 * 60 * 60;

/// "v0.9.36" → [0, 9, 36]; None for anything that isn't dotted numbers.
fn parse(v: &str) -> Option<Vec<u32>> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

fn newer(tag: &str, current: &str) -> bool {
    matches!((parse(tag), parse(current)), (Some(t), Some(c)) if t > c)
}

pub fn check(toasts: adw::ToastOverlay) {
    let s = crate::prefs::get();
    let now = glib::real_time() as u64 / 1_000_000;
    if !s.check_updates || now.saturating_sub(s.last_update_check) < DAY {
        return;
    }
    // stamped before the request, so other windows (and a failed check) wait a day too
    crate::prefs::update(|s| s.last_update_check = now);
    let Ok(p) = gio::Subprocess::newv(
        &["curl", "-s", "--max-time", "10", "-o", "/dev/null", "-w", "%{redirect_url}", LATEST].map(std::ffi::OsStr::new),
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    ) else {
        return; // no curl: stay quiet
    };
    glib::spawn_future_local(async move {
        let Ok((Some(out), _)) = p.communicate_utf8_future(None).await else { return };
        let url = out.trim().to_string();
        let Some(tag) = url.rsplit_once("/tag/").map(|(_, t)| t.to_string()) else { return };
        if !newer(&tag, env!("CARGO_PKG_VERSION")) {
            return;
        }
        let t = adw::Toast::builder()
            .title(format!("Kestrel {} is available", tag.trim_start_matches('v')))
            .button_label("View Release")
            .timeout(0)
            .build();
        t.connect_button_clicked(move |_| gtk::UriLauncher::new(&url).launch(None::<&gtk::Window>, None::<&gio::Cancellable>, |_| {}));
        toasts.add_toast(t);
    });
}

#[cfg(test)]
mod tests {
    use super::newer;
    #[test]
    fn compares_numerically() {
        assert!(newer("v0.9.36", "0.9.35"));
        assert!(newer("v0.10.0", "0.9.35"));
        assert!(newer("v1.0.0", "0.9.35"));
        assert!(!newer("v0.9.35", "0.9.35"));
        assert!(!newer("v0.9.4", "0.9.35"));
        assert!(!newer("vnightly", "0.9.35"));
    }
}
