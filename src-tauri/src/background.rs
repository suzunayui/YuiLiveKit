//! Running without the window, for when OBS starts the app.
//!
//! The OBS plugin starts the app with `--background` when OBS starts, and with
//! `--quit-background` when OBS closes. The app then runs with its window hidden, reachable
//! from the tray icon, and goes away with OBS unless the user has the window open. The rules
//! are here, apart from the window code, so they can be tested.

use serde::{Deserialize, Serialize};

pub const BACKGROUND_ARG: &str = "--background";
pub const QUIT_ARG: &str = "--quit-background";

/// What OBS may ask of the app. Both are off until the user turns them on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObsLink {
    /// The plugin starts the app, hidden, when OBS starts, and closes it when OBS closes.
    pub start_with_obs: bool,
    /// Started that way, the app collects chat from where it last did.
    pub auto_start: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Launch {
    /// The user opened the app.
    Normal,
    /// OBS started; keep running without the window.
    Background,
    /// OBS closed; go away unless the user is using the window.
    QuitBackground,
}

pub fn launch_kind<S: AsRef<str>>(args: &[S]) -> Launch {
    if args.iter().any(|a| a.as_ref() == QUIT_ARG) {
        Launch::QuitBackground
    } else if args.iter().any(|a| a.as_ref() == BACKGROUND_ARG) {
        Launch::Background
    } else {
        Launch::Normal
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    ShowWindow,
    Nothing,
    Exit,
}

/// What an already running app does when it is launched again. `kept_for_obs` says OBS wants it
/// kept running; the second value is whether that still holds afterwards.
pub fn on_second_launch(kind: Launch, kept_for_obs: bool, window_visible: bool) -> (Action, bool) {
    match kind {
        Launch::Normal => (Action::ShowWindow, kept_for_obs),
        Launch::Background => (Action::Nothing, true),
        // With the window open the user is using the app, so it stays; from then on, closing
        // the window closes the app, as it does when OBS was never involved.
        Launch::QuitBackground if kept_for_obs && !window_visible => (Action::Exit, false),
        Launch::QuitBackground => (Action::Nothing, false),
    }
}

/// Whether closing the window should only hide it: while OBS wants the app kept running.
pub fn hide_on_close(kept_for_obs: bool) -> bool {
    kept_for_obs
}

/// Whether to begin collecting chat by itself: only when OBS started the app, the user asked
/// for it, and there is somewhere to collect from.
pub fn should_auto_start(kind: Launch, link: ObsLink, has_connection: bool) -> bool {
    kind == Launch::Background && link.auto_start && has_connection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_say_who_started_the_app() {
        assert_eq!(launch_kind::<&str>(&[]), Launch::Normal);
        assert_eq!(launch_kind(&["YuiLiveKit.exe"]), Launch::Normal);
        assert_eq!(launch_kind(&["YuiLiveKit.exe", "--background"]), Launch::Background);
        assert_eq!(launch_kind(&["YuiLiveKit.exe", "--quit-background"]), Launch::QuitBackground);
        assert_eq!(launch_kind(&["x", "--background", "--quit-background"]), Launch::QuitBackground);
    }

    #[test]
    fn obs_closing_ends_the_app_only_when_nobody_is_looking_at_it() {
        // OBS started it, the window was never opened, OBS closes: the app goes too.
        assert_eq!(on_second_launch(Launch::QuitBackground, true, false), (Action::Exit, false));
        // The settings window is open: the app stays, and is the user's from then on.
        assert_eq!(on_second_launch(Launch::QuitBackground, true, true), (Action::Nothing, false));
        assert!(!hide_on_close(false));
        // The user started it and OBS was never involved: OBS closing changes nothing.
        assert_eq!(on_second_launch(Launch::QuitBackground, false, false), (Action::Nothing, false));
        assert_eq!(on_second_launch(Launch::QuitBackground, false, true), (Action::Nothing, false));
    }

    #[test]
    fn obs_starting_keeps_a_running_app_and_opening_it_shows_the_window() {
        // Already open when OBS starts: nothing moves, but it now stays for OBS when closed.
        assert_eq!(on_second_launch(Launch::Background, false, true), (Action::Nothing, true));
        assert!(hide_on_close(true));
        // Opening the app while it runs hidden shows the window and changes nothing else.
        assert_eq!(on_second_launch(Launch::Normal, true, false), (Action::ShowWindow, true));
        assert_eq!(on_second_launch(Launch::Normal, false, true), (Action::ShowWindow, false));
    }

    #[test]
    fn collecting_starts_by_itself_only_when_asked_and_only_for_obs() {
        let on = ObsLink { start_with_obs: true, auto_start: true };
        let off = ObsLink { start_with_obs: true, auto_start: false };
        assert!(should_auto_start(Launch::Background, on, true));
        assert!(!should_auto_start(Launch::Background, off, true), "not unless the user turned it on");
        assert!(!should_auto_start(Launch::Background, on, false), "not without somewhere to collect from");
        assert!(!should_auto_start(Launch::Normal, on, true), "not when the user opens the app");
        assert_eq!(ObsLink::default(), ObsLink { start_with_obs: false, auto_start: false }, "both are off at first");
    }
}
