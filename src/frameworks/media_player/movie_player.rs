/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMoviePlayerController` etc.

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::foundation::{ns_string, ns_url, NSInteger, NSTimeInterval};
use crate::frameworks::uikit::ui_device::UIDeviceOrientation;
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, todo_objc_setter, ClassExports,
    HostObject, NSZonePtr,
};
use crate::Environment;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct State {
    active_player: Option<id>,
    /// Various apps (e.g. Crash Bandicoot Nitro Kart 3D and Spore Origins)
    /// create or start a player and await some kind of notification, but can't
    /// handle it if that notification happens immediately. This queue lets us
    /// delay such notifications until the app next returns to the run loop,
    /// which seems to be late enough.
    ///
    /// The last field says whether the runtime's retain of the object (taken
    /// by `play`) is to be released once the notification has been posted.
    pending_notifications: VecDeque<(&'static str, id, Instant, bool)>,
    /// n72 fork: the movie playing in n72-player, and the fingers that went
    /// down while it played (theirs are not the app's touches).
    #[cfg(feature = "cpu-native")]
    movie: Option<n72::Movie>,
    #[cfg(feature = "cpu-native")]
    skip_fingers: std::collections::HashSet<crate::window::FingerId>,
}
impl State {
    fn get(env: &mut Environment) -> &mut Self {
        &mut env.framework_state.media_player.movie_player
    }
}

type MPMovieScalingMode = NSInteger;
type MPMovieControlStyle = NSInteger;

type MPMoviePlaybackState = NSInteger;
const MPMoviePlaybackStateStopped: MPMoviePlaybackState = 0;
#[cfg(feature = "cpu-native")]
const MPMoviePlaybackStatePlaying: MPMoviePlaybackState = 1;

// Values might not be correct, but as these are linked symbol constants, it
// shouldn't matter.
pub const MPMoviePlayerPlaybackDidFinishNotification: &str =
    "MPMoviePlayerPlaybackDidFinishNotification";
/// Apparently an undocumented, private API. Spore Origins uses it.
pub const MPMoviePlayerContentPreloadDidFinishNotification: &str =
    "MPMoviePlayerContentPreloadDidFinishNotification";
pub const MPMoviePlayerScalingModeDidChangeNotification: &str =
    "MPMoviePlayerScalingModeDidChangeNotification";
// TODO: More notifications?
const MPMoviePlayerPlaybackDidFinishReasonUserInfoKey: &str =
    "MPMoviePlayerPlaybackDidFinishReasonUserInfoKey";

/// `NSNotificationName` values and other constants.
pub const CONSTANTS: ConstantExports = &[
    (
        "_MPMoviePlayerPlaybackDidFinishNotification",
        HostConstant::NSString(MPMoviePlayerPlaybackDidFinishNotification),
    ),
    (
        "_MPMoviePlayerContentPreloadDidFinishNotification",
        HostConstant::NSString(MPMoviePlayerContentPreloadDidFinishNotification),
    ),
    (
        "_MPMoviePlayerScalingModeDidChangeNotification",
        HostConstant::NSString(MPMoviePlayerScalingModeDidChangeNotification),
    ),
    (
        "_MPMoviePlayerPlaybackDidFinishReasonUserInfoKey",
        HostConstant::NSString(MPMoviePlayerPlaybackDidFinishReasonUserInfoKey),
    ),
];

struct MPMoviePlayerControllerHostObject {
    // NSURL *
    content_url: id,
    /// From the undocumented `setOrientation:animated:`.
    #[cfg_attr(not(feature = "cpu-native"), allow(dead_code))]
    orientation: Option<UIDeviceOrientation>,
}
impl HostObject for MPMoviePlayerControllerHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMoviePlayerController: NSObject

// TODO: actual playback

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(MPMoviePlayerControllerHostObject {
        content_url: nil,
        orientation: None,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithContentURL:(id)url { // NSURL*
    log!(
        "TODO: [(MPMoviePlayerController*){:?} initWithContentURL:{:?} ({:?})]",
        this,
        url,
        ns_url::to_rust_path(env, url),
    );

    retain(env, url);
    env.objc.borrow_mut::<MPMoviePlayerControllerHostObject>(this).content_url = url;

    // Act as if loading immediately completed (Spore Origins waits for this).
    State::get(env).pending_notifications.push_back(
        (MPMoviePlayerContentPreloadDidFinishNotification, this, Instant::now(), false)
    );

    this
}

- (())dealloc {
    let url = env.objc.borrow::<MPMoviePlayerControllerHostObject>(this).content_url;
    release(env, url);

    env.objc.dealloc_object(this, &mut env.mem);
}

- (id)contentURL {
    env.objc.borrow::<MPMoviePlayerControllerHostObject>(this).content_url
}

- (id)backgroundColor {
    msg_class![env; UIColor blackColor] // TODO
}
- (())setBackgroundColor:(id)color { // UIColor*
    todo_objc_setter!(this, color);
}

- (())setScalingMode:(MPMovieScalingMode)mode {
    todo_objc_setter!(this, mode);
}
- (())setUseApplicationAudioSession:(bool)use_session {
    todo_objc_setter!(this, use_session);
}
- (())setControlStyle:(MPMovieControlStyle)style {
    todo_objc_setter!(this, style);
}
- (())setFullscreen:(bool)fullsreen {
    todo_objc_setter!(this, fullsreen);
}
- (())setInitialPlaybackTime:(NSTimeInterval)initial_time {
    todo_objc_setter!(this, initial_time);
}

- (id)view {
    nil // TODO
}

- (MPMoviePlaybackState)playbackState {
    #[cfg(feature = "cpu-native")]
    if State::get(env).movie.as_ref().is_some_and(|m| m.player == this) {
        return MPMoviePlaybackStatePlaying;
    }
    MPMoviePlaybackStateStopped // TODO: paused etc.
}

// Apparently an undocumented, private API, but Spore Origins uses it.
- (())setMovieControlMode:(NSInteger)_mode {
    // As this is undocumented and we don't have real video playback yet, let's
    // ignore it.
}

// Another undocumented one! But some apps may still use it :/
// https://stackoverflow.com/a/1390079/2241008
- (())setOrientation:(UIDeviceOrientation)orientation animated:(bool)_animated {
    env.objc.borrow_mut::<MPMoviePlayerControllerHostObject>(this).orientation = Some(orientation);
}

// MPMediaPlayback implementation
- (())play {
    // n72 fork: playing already (NOVA2 calls `play` twice): nothing to do.
    #[cfg(feature = "cpu-native")]
    if State::get(env).movie.as_ref().is_some_and(|m| m.player == this) {
        return;
    }
    #[cfg(not(feature = "cpu-native"))]
    log!("TODO: [(MPMoviePlayerController*){:?} play]", this);
    if let Some(old) = env.framework_state.media_player.movie_player.active_player {
        let _: () = msg![env; old stop];
    }
    assert!(env.framework_state.media_player.movie_player.active_player.is_none());
    // Movie player is retained by the runtime until it is stopped
    retain(env, this);
    env.framework_state.media_player.movie_player.active_player = Some(this);

    // n72 fork: the movie in n72-player; handle_players posts the
    // notification when it ends. If it cannot be played, as before.
    #[cfg(feature = "cpu-native")]
    if n72::start(env, this) {
        return;
    }

    // Act as if playback immediately completed after 1 second
    // (various apps wait for this, such as BIA and Hero of Sparta).
    let notif = (MPMoviePlayerPlaybackDidFinishNotification, this, Instant::now().checked_add(Duration::from_millis(1000)).unwrap(), false);
    for (name, obj, _, _) in &mut State::get(env).pending_notifications {
        // De-duplicate similar notifications. This can happen if app is calling
        // `play` twice on the same player object (case of NOVA2).
        if *name == MPMoviePlayerPlaybackDidFinishNotification && *obj == this {
            return;
        }
    }
    State::get(env).pending_notifications.push_back(notif);
}

- (())pause {
    log!("TODO: [(MPMoviePlayerController*){:?} pause]", this);
}

- (())stop {
    // n72 fork: the movie ends now; the notification follows from the run
    // loop (not from here: apps call `stop` in their handler of it), and the
    // player stays retained until it has been posted.
    #[cfg(feature = "cpu-native")]
    if State::get(env).movie.as_ref().is_some_and(|m| m.player == this) {
        n72::end(env, "stopped by the app");
        assert!(State::get(env).active_player.take() == Some(this));
        State::get(env).pending_notifications.push_back(
            (MPMoviePlayerPlaybackDidFinishNotification, this, Instant::now(), true)
        );
        return;
    }
    #[cfg(not(feature = "cpu-native"))]
    log!("TODO: [(MPMoviePlayerController*){:?} stop]", this);
    if env.framework_state.media_player.movie_player.active_player.is_some() {
        // Some applications (like NOVA2) may send 2 `stop` messages for each
        // 1 `play` message for the player. In that case, we want to release
        // the active player only once.
        assert!(this == env.framework_state.media_player.movie_player.active_player.take().unwrap());
        release(env, this);
    }
}

@end

@implementation MPMoviePlayerViewController: UIViewController

- (id)initWithContentURL:(id)url {
    log!(
        "TODO: [(MPMoviePlayerViewController*){:?} initWithContentURL:{:?} ({:?})] -> nil",
        this,
        url,
        ns_url::to_rust_path(env, url),
    );
    release(env, this);
    nil // TODO
}

@end

};

/// For use by `NSRunLoop` via [super::handle_players]: check movie players'
/// status, send notifications if necessary.
pub(super) fn handle_players(env: &mut Environment) {
    // n72 fork: the movie has ended (played out, or skipped by a touch).
    #[cfg(feature = "cpu-native")]
    if let Some(player) = n72::poll(env) {
        assert!(State::get(env).active_player.take() == Some(player));
        State::get(env).pending_notifications.push_back((
            MPMoviePlayerPlaybackDidFinishNotification,
            player,
            Instant::now(),
            true,
        ));
    }

    let mut notifs_to_run = Vec::new();
    let pending_notifs = &mut State::get(env).pending_notifications;
    let mut i = 0;
    while i < pending_notifs.len() {
        let (name_str, object, time, release_after) = pending_notifs[i];
        if Instant::now() >= time {
            notifs_to_run.push((name_str, object, release_after));
            pending_notifs.swap_remove_back(i);
        } else {
            i += 1;
        }
    }
    for (name_str, object, release_after) in notifs_to_run {
        let name = ns_string::get_static_str(env, name_str);
        let center: id = msg_class![env; NSNotificationCenter defaultCenter];
        // TODO: should there be some user info attached?
        let _: () = msg![env; center postNotificationName:name object:object];
        if release_after {
            release(env, object);
        }
    }
}

/// n72 fork: for [crate::frameworks::uikit::handle_events]: a touch while a
/// movie plays ends it (on iPhone OS a tap shows the controls with Done), and
/// that finger's touch goes to the movie, not to the app, until it lifts.
/// Returns the event (perhaps with fewer fingers) if anything of it is left
/// for the app.
#[cfg(feature = "cpu-native")]
pub fn n72_movie_touch(
    env: &mut Environment,
    event: crate::window::Event,
) -> Option<crate::window::Event> {
    use crate::window::Event;
    let state = State::get(env);
    match event {
        Event::TouchesDown(map) if state.movie.is_some() => {
            state.skip_fingers.extend(map.keys().copied());
            n72::skip(env);
            None
        }
        Event::TouchesMove(mut map) if !state.skip_fingers.is_empty() => {
            map.retain(|finger, _| !state.skip_fingers.contains(finger));
            (!map.is_empty()).then_some(Event::TouchesMove(map))
        }
        Event::TouchesUp(mut map) if !state.skip_fingers.is_empty() => {
            map.retain(|finger, _| !state.skip_fingers.remove(finger));
            (!map.is_empty()).then_some(Event::TouchesUp(map))
        }
        other => Some(other),
    }
}

#[cfg(feature = "cpu-native")]
mod n72 {
    //! Movies through n72-player (n72-alpine pkg/n72-tools/player): the
    //! iPod's hardware decoders, its pictures on the display's video plane
    //! (above touchHLE's layer 0, below the launcher's transparent layer 1),
    //! its sound to ALSA's default (dmix, beside touchHLE's OpenAL).

    use super::State;
    use crate::frameworks::foundation::ns_url;
    use crate::frameworks::uikit::ui_device::{
        UIDeviceOrientation, UIDeviceOrientationLandscapeLeft, UIDeviceOrientationLandscapeRight,
        UIDeviceOrientationPortrait,
    };
    use crate::objc::{id, nil};
    use crate::window::DeviceOrientation;
    use crate::Environment;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    pub struct Movie {
        pub player: id,
        child: Child,
        /// Why it ends, once it has been told to.
        ending: Option<&'static str>,
    }

    /// n72-player's turn (LVGL's: 90 puts the picture's top at the panel's
    /// left edge, the iPod held with its home button on the left; 270 home
    /// button right; 0 upright) for the movie.
    ///
    /// iPhone OS 3 plays movies in landscape, home button right
    /// (UIInterfaceOrientationLandscapeRight) unless the app asks otherwise
    /// with `setOrientation:animated:`; a landscape app's own side is taken
    /// as the one it is held on. (DeviceOrientation and UIDeviceOrientation
    /// name the way the device is turned: LandscapeLeft is home button
    /// right.)
    fn turn(orientation: Option<UIDeviceOrientation>, app: DeviceOrientation) -> u32 {
        const DEFAULT: u32 = 270;
        match orientation {
            Some(UIDeviceOrientationPortrait) => 0,
            Some(UIDeviceOrientationLandscapeLeft) => 270,
            Some(UIDeviceOrientationLandscapeRight) => 90,
            _ => match app {
                DeviceOrientation::LandscapeLeft => 270,
                DeviceOrientation::LandscapeRight => 90,
                _ => DEFAULT,
            },
        }
    }

    /// Start the movie of `player` (retained, the active player). False if
    /// it cannot be played here (not a file on the host, no display, no
    /// player): the caller goes on as without playback.
    pub fn start(env: &mut Environment, player: id) -> bool {
        let host = env
            .objc
            .borrow::<super::MPMoviePlayerControllerHostObject>(player);
        let (url, orientation) = (host.content_url, host.orientation);
        if url == nil {
            return false;
        }
        let guest_path = ns_url::to_rust_path(env, url);
        let Some(path) = env
            .fs
            .host_file_path(&guest_path)
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned()))
        else {
            log!(
                "n72: movie {:?} is not a file on the host (in an .ipa?), not played",
                guest_path
            );
            return false;
        };
        let Some(window) = env.window.as_mut() else {
            return false;
        };
        let rot = turn(orientation, window.current_rotation());
        if !window.n72_lend_display(true) {
            return false;
        }
        let mut cmd = Command::new("n72-player");
        cmd.arg(&path)
            .env("N72_PLAYER_ROT", rot.to_string())
            .stdin(Stdio::null());
        // Ended with touchHLE however it goes (it stays in touchHLE's process
        // group too, which the launcher's Home ends).
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(child) => {
                log!(
                    "n72: movie {} playing in n72-player, pid {}, turn {}",
                    path.display(),
                    child.id(),
                    rot
                );
                State::get(env).movie = Some(Movie {
                    player,
                    child,
                    ending: None,
                });
                true
            }
            Err(e) => {
                log!(
                    "n72: movie {}: n72-player not started: {}",
                    path.display(),
                    e
                );
                env.window.as_mut().unwrap().n72_lend_display(false);
                false
            }
        }
    }

    /// A touch: end the movie (the run loop sees the player exit).
    pub fn skip(env: &mut Environment) {
        let Some(movie) = State::get(env).movie.as_mut() else {
            return;
        };
        if movie.ending.is_none() {
            movie.ending = Some("skipped by a touch");
            unsafe { libc::kill(movie.child.id() as _, libc::SIGTERM) };
        }
    }

    /// The player that played the movie, once the movie has ended.
    pub fn poll(env: &mut Environment) -> Option<id> {
        let movie = State::get(env).movie.as_mut()?;
        let status = match movie.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => format!("{status}"),
            Err(e) => format!("{e}"),
        };
        let movie = State::get(env).movie.take().unwrap();
        finished(env, &movie, &status, movie.ending.unwrap_or("played out"));
        Some(movie.player)
    }

    /// End the movie now: n72-player is asked to quit (SIGTERM: it stops
    /// its pipeline and takes its picture off the screen), and killed if it
    /// has not within 2 s.
    pub fn end(env: &mut Environment, why: &'static str) {
        let Some(mut movie) = State::get(env).movie.take() else {
            return;
        };
        unsafe { libc::kill(movie.child.id() as _, libc::SIGTERM) };
        let give_up = Instant::now() + Duration::from_secs(2);
        let status = loop {
            match movie.child.try_wait() {
                Ok(Some(status)) => break format!("{status}"),
                Ok(None) if Instant::now() < give_up => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    let _ = movie.child.kill();
                    break match movie.child.wait() {
                        Ok(status) => format!("{status}, killed after 2 s"),
                        Err(e) => format!("{e}"),
                    };
                }
                Err(e) => break format!("{e}"),
            }
        };
        finished(env, &movie, &status, why);
    }

    fn finished(env: &mut Environment, movie: &Movie, status: &str, why: &str) {
        log!(
            "n72: movie ended ({}), n72-player pid {}: {}",
            why,
            movie.child.id(),
            status
        );
        if let Some(window) = env.window.as_mut() {
            window.n72_lend_display(false);
        }
    }
}
