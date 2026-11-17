/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITouch`.

use super::ui_event;
use crate::frameworks::core_graphics::{CGPoint, CGRect};
use crate::frameworks::foundation::{NSInteger, NSTimeInterval, NSUInteger};
use crate::mem::MutVoidPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};
use crate::window::{Coords, Event, FingerId};
use crate::Environment;
use std::collections::hash_map::{Entry, HashMap};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub type UITouchPhase = NSInteger;
pub const UITouchPhaseBegan: UITouchPhase = 0;
pub const UITouchPhaseMoved: UITouchPhase = 1;
pub const UITouchPhaseStationary: UITouchPhase = 2;
pub const UITouchPhaseEnded: UITouchPhase = 3;

#[derive(Default)]
pub struct State {
    current_touches: HashMap<FingerId, id>,
    /// n72 fork: frames the app has presented (EAGLContext
    /// presentRenderbuffer), and when it last did.
    pub frames_presented: u64,
    pub last_present: Option<Instant>,
    /// n72 fork: each finger down: see [FingerDown].
    down_at: HashMap<FingerId, FingerDown>,
}

/// n72 fork: a finger the app got a touchesBegan for.
struct FingerDown {
    /// the frame count and time then
    frame: u64,
    at: Instant,
    /// where it went down, and whether it has left the slop circle around it
    coords: Coords,
    moved: bool,
}

/// n72 fork: how far a finger must move from where it went down before the
/// app hears of any movement (points; N72_TOUCH_SLOP overrides it). The
/// N72's panel reports a tap as 1.5 to 4.5 points of movement, mostly as the
/// finger lifts and the contact shrinks (measured in Spore).
/// Spore takes that movement as a scroll, though taps work on a real iPod,
/// so the OS touch stack shows apps less of it (UIKit itself passes on
/// any movement it gets). A tap's up is reported where it went down. The
/// cost: drags start that late, and smaller slow drags are lost. n72-ui's
/// touch router (a22d60b) holds the position while a resting contact
/// collapses at lift-off; through it the recorded taps move 0.1 to 1.8
/// points, so 3 points is enough (8 without that router).
fn touch_slop() -> f32 {
    static SLOP: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SLOP.get_or_init(|| {
        std::env::var("N72_TOUCH_SLOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3.0)
    })
}

/// n72 fork: how long a touch-up may wait for the app to draw a frame with
/// the finger down. A game that reads touches once per frame (Spore) misses a
/// tap whose down and up both arrive between two of its frames: touchHLE
/// hands it every queued event at once, and at 20 FPS a quick tap fits in a
/// frame. The real OS delivers each event when it happens.
const TAP_HOLD_MAX: Duration = Duration::from_millis(250);

/// n72 fork: for [super::handle_events]: should this touch-up wait, because
/// a finger in it went down after the app's last frame? Only while the app is
/// drawing frames (an app that presents none would only get later ups).
pub fn should_hold_up(env: &Environment, map: &HashMap<FingerId, Coords>) -> Option<Instant> {
    let state = &env.framework_state.uikit.ui_touch;
    let now = Instant::now();
    let drawing = state
        .last_present
        .is_some_and(|t| now.duration_since(t) < TAP_HOLD_MAX);
    if !drawing {
        return None;
    }
    map.keys()
        .filter_map(|finger| state.down_at.get(finger))
        .filter(|d| d.frame == state.frames_presented && now.duration_since(d.at) < TAP_HOLD_MAX)
        .map(|d| d.at + TAP_HOLD_MAX)
        .min()
}

pub(super) struct UITouchHostObject {
    /// Strong reference to the `UIView`
    pub(super) view: id,
    /// Strong reference to the `UIWindow`, used as a reference for co-ordinate
    /// space conversion
    pub(super) window: id,
    /// Relative to the screen
    location: CGPoint,
    /// Relative to the screen
    previous_location: CGPoint,
    timestamp: NSTimeInterval,
    phase: UITouchPhase,
}
impl HostObject for UITouchHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITouch: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UITouchHostObject {
        view: nil,
        window: nil,
        location: CGPoint { x: 0.0, y: 0.0 },
        previous_location: CGPoint { x: 0.0, y: 0.0 },
        timestamp: 0.0,
        phase: UITouchPhaseBegan,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())dealloc {
    let &mut UITouchHostObject { view, window, .. } = env.objc.borrow_mut(this);
    release(env, view);
    release(env, window);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (CGPoint)locationInView:(id)that_view { // UIView*
    let &UITouchHostObject { location, window, .. } = env.objc.borrow(this);
    let location_in_window: CGPoint = msg![env; window convertPoint:location fromWindow:nil];
    if that_view == nil {
        location_in_window
    } else {
        msg![env; that_view convertPoint:location_in_window fromView:window]
    }
}
- (CGPoint)previousLocationInView:(id)that_view { // UIView*
    let &UITouchHostObject { previous_location, window, .. } = env.objc.borrow(this);
    let location_in_window: CGPoint = msg![env; window convertPoint:previous_location fromWindow:nil];
    if that_view == nil {
        location_in_window
    } else {
        msg![env; that_view convertPoint:location_in_window fromView:window]
    }
}

- (id)view {
    env.objc.borrow::<UITouchHostObject>(this).view
}

- (NSTimeInterval)timestamp {
    env.objc.borrow::<UITouchHostObject>(this).timestamp
}

- (NSUInteger)tapCount {
    1 // TODO: support double-taps etc
}

- (UITouchPhase)phase {
    env.objc.borrow::<UITouchHostObject>(this).phase
}

@end

};

/// [super::handle_events] will forward touch events to this function.
pub fn handle_event(env: &mut Environment, mut event: Event) {
    // n72 fork: the frame a finger went down in, and the touch slop.
    {
        let state = &mut env.framework_state.uikit.ui_touch;
        let frame = state.frames_presented;
        let slop = touch_slop();
        match &mut event {
            Event::TouchesDown(map) => {
                for (&finger, &coords) in map.iter() {
                    state.down_at.entry(finger).or_insert(FingerDown {
                        frame,
                        at: Instant::now(),
                        coords,
                        moved: false,
                    });
                }
            }
            Event::TouchesMove(map) => {
                map.retain(|finger, coords| {
                    let Some(d) = state.down_at.get_mut(finger) else {
                        return true;
                    };
                    if !d.moved {
                        let (dx, dy) = (coords.0 - d.coords.0, coords.1 - d.coords.1);
                        d.moved = dx * dx + dy * dy > slop * slop;
                    }
                    d.moved
                });
                if map.is_empty() {
                    return;
                }
            }
            Event::TouchesUp(map) => {
                for (finger, coords) in map.iter_mut() {
                    if let Some(d) = state.down_at.remove(finger) {
                        if !d.moved {
                            *coords = d.coords;
                        }
                    }
                }
            }
            _ => (),
        }
    }
    #[cfg(feature = "n72-prof")]
    // n72 fork: N72_LOG_TOUCH=1 logs what reaches the app (a drag must show
    // as one down, moves, one up; a touch screen in multitouch protocol A
    // came through SDL as a down and an up per frame).
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("N72_LOG_TOUCH").is_some_and(|v| v != "0")) {
            let (kind, map) = match &event {
                Event::TouchesDown(map) => ("down", map),
                Event::TouchesMove(map) => ("move", map),
                Event::TouchesUp(map) => ("up", map),
                _ => unreachable!(),
            };
            for (finger, coords) in map {
                log!(
                    "n72 touch {} {:?} at {:.0}, {:.0}, frame {}",
                    kind,
                    finger,
                    coords.0,
                    coords.1,
                    env.framework_state.uikit.ui_touch.frames_presented
                );
            }
        }
    }
    // before processing anything, we mark all current touches as stationary
    let current_touches = &env.framework_state.uikit.ui_touch.current_touches;
    for &touch in (*current_touches).values() {
        env.objc.borrow_mut::<UITouchHostObject>(touch).phase = UITouchPhaseStationary;
    }
    match event {
        Event::TouchesDown(map) => handle_touches_down(env, map),
        Event::TouchesMove(map) => handle_touches_move(env, map),
        Event::TouchesUp(map) => handle_touches_up(env, map),
        _ => unreachable!(),
    }
}

fn handle_touches_down(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    // UIKit creates and drains autorelease pools when handling events.
    let pool: id = msg_class![env; NSAutoreleasePool new];

    // Note: if the emulator is heavily lagging, this timestamp is going
    // to be far off from the truth, since it should represent the
    // time when the event actually happened, not the time when the
    // event was dispatched. Maybe we'll need to fix this eventually.
    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    for (finger_id, coords) in map {
        let current_touches = &mut env.framework_state.uikit.ui_touch.current_touches;

        if current_touches.contains_key(&finger_id) {
            // this seems to happen only on the desktop with a single touch
            assert_eq!(current_touches.len(), 1);
            log!(
                "Warning: New touch {:?} initiated but current touch did not end yet, treating as movement.",
                finger_id
            );
            return handle_touches_move(env, HashMap::from([(finger_id, coords)]));
        }

        log_dbg!("Finger {:?} touch down: {:?}", finger_id, coords);

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        // TODO: is this the correct state of the UITouch and UIEvent during
        //       hit testing?

        let new_touch: id = msg_class![env; UITouch alloc];
        *env.objc.borrow_mut(new_touch) = UITouchHostObject {
            view: nil,
            window: nil,
            location,
            previous_location: location,
            timestamp,
            phase: UITouchPhaseBegan,
        };
        autorelease(env, new_touch);

        let _: () = msg![env; touches addObject:new_touch];

        let _ = &env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .insert(finger_id, new_touch);
        retain(env, new_touch);
    }

    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    // views with existing touches (see isMultipleTouchEnabled check below)
    let views_with_existing_touches: HashSet<id> = env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .values()
        .map(|&touch| env.objc.borrow::<UITouchHostObject>(touch).view)
        .collect();

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    let touches_arr: id = msg![env; touches allObjects];
    let touches_count: NSUInteger = msg![env; touches_arr count];
    for i in 0..touches_count {
        let touch: id = msg![env; touches_arr objectAtIndex:i];
        let &UITouchHostObject { location, .. } = env.objc.borrow(touch);

        // Assumes the windows in the list are ordered back-to-front.
        // TODO: this may not be correct once we support windowLevel.
        let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
        let Some((window, location_in_window)) = windows.into_iter().rev().find_map(|window| {
            let location_in_window: CGPoint =
                msg![env; window convertPoint:location fromWindow:nil];
            if msg![env; window pointInside:location_in_window withEvent:event] {
                Some((window, location_in_window))
            } else {
                None
            }
        }) else {
            log!(
                "Couldn't find a window for touch at {:?}, discarding",
                location,
            );
            continue;
        };

        let view: id = msg![env; window hitTest:location_in_window withEvent:event];
        if view == nil {
            log!(
                "Couldn't find a view for touch at {:?} in window {:?}, discarding",
                location_in_window,
                window,
            );
            continue;
        } else {
            log_dbg!(
                "Found view {:?} with frame {:?} for touch at {:?} in window {:?}",
                view,
                {
                    let f: CGRect = msg![env; view frame];
                    f
                },
                location_in_window,
                window,
            );
        }

        let is_multi_touch_enabled: bool = msg![env; view isMultipleTouchEnabled];
        if !is_multi_touch_enabled {
            // When a view has multi-touch disabled, it can only have one active
            // touch at once. So, we can only report a new touch to the view if
            // there are no other touches currently associated with it, and if
            // there are multiple new touches for this view, we can only report
            // one of them.
            let view_has_other_new_touches = view_touches.contains_key(&view);
            let view_has_existing_touches = views_with_existing_touches.contains(&view);
            if view_has_other_new_touches || view_has_existing_touches {
                log!(
                    "Ignoring new touch {:?} for view {:?}, !isMultipleTouchEnabled",
                    touch,
                    view
                );
                // The touch will continue to be tracked until it ends, but the
                // view will be nil, so messages sent to it will be ignored.
                // TODO: Figure out if/how these should be delivered elsewhere
                //       in the responder chain.
                // FIXME: The fact the view is nil might be observed via
                //        touchesForView:nil or allTouches on UIEvent.
                //        This might cause problems. What does the real OS do?
                //        Does this need to be prevented?
                continue;
            }
        }

        // Only create the set after the isMultipleTouchEnabled checks so we
        // won't end up with an empty set.
        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];

        retain(env, view);
        retain(env, window);
        {
            let new_touch = env.objc.borrow_mut::<UITouchHostObject>(touch);
            new_touch.view = view;
            new_touch.window = window;
        }
    }

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesBegan:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesBegan:touches withEvent:event];
    }

    release(env, pool);
}

fn handle_touches_move(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    for (finger_id, coords) in map {
        let Some(&touch) = env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .get(&finger_id)
        else {
            log!(
                "Warning: Finger {:?} touch move event received but no current touch, ignoring.",
                finger_id
            );
            continue;
        };

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        let view = env.objc.borrow::<UITouchHostObject>(touch).view;
        let host_object = env.objc.borrow_mut::<UITouchHostObject>(touch);

        if host_object.location == location {
            continue;
        }

        log_dbg!("Finger {:?} touch move: {:?}", finger_id, coords);

        host_object.previous_location = host_object.location;
        host_object.location = location;
        host_object.timestamp = timestamp;
        assert_eq!(host_object.phase, UITouchPhaseStationary);
        host_object.phase = UITouchPhaseMoved;

        let _: () = msg![env; touches addObject:touch];

        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];
    }

    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesMoved:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesMoved:touches withEvent:event];
    }

    release(env, pool);
}

fn handle_touches_up(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    // We need to construct all touches set _BEFORE_ removing touches!
    // (as removed one are reported as the part of the event)
    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    for (finger_id, coords) in map {
        let Some(&touch) = env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .get(&finger_id)
        else {
            log!(
                "Warning: Finger {:?} touch up event received but no current touch, ignoring.",
                finger_id
            );
            continue;
        };

        log_dbg!("Finger {:?} touch up: {:?}", finger_id, coords);

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        let view = env.objc.borrow::<UITouchHostObject>(touch).view;
        let host_object = env.objc.borrow_mut::<UITouchHostObject>(touch);
        host_object.previous_location = host_object.location;
        host_object.location = location;
        host_object.timestamp = timestamp;
        assert_eq!(host_object.phase, UITouchPhaseStationary);
        host_object.phase = UITouchPhaseEnded;

        let _: () = msg![env; touches addObject:touch];

        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];

        let _ = &env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .remove(&finger_id);
        release(env, touch); // only owner now should be the NSSet
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesEnded:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesEnded:touches withEvent:event];
    }

    release(env, pool);
}
