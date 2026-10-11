#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <X11/XKBlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XInput2.h>
#include <X11/extensions/XTest.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static Display *d;
static Window root, clock_window;
static Atom protocols, take_focus, active_window, clock_atom;
static unsigned offers, invalid_offers;
static Time offer_time;
static Window offer_window;
static int answer_offers;
static Window key_target;
static KeyCode keys[4];
static unsigned presses[4], releases[4], raw_presses[4], raw_releases[4];
static unsigned wrong_target, wrong_modifiers, modifiers, raw_motion;
static int xi_opcode;
static Time motion_time;
static void fullscreen(Window win, int enabled);

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void pause_events(void) {
    struct timespec t = {0, 2000000};
    nanosleep(&t, NULL);
}

static void check(int condition, const char *name) {
    printf("%s: %s\n", condition ? "PASS" : "FAIL", name);
    if (!condition) exit(1);
}

static Window focused(void) {
    Window win;
    int revert;
    XGetInputFocus(d, &win, &revert);
    return win;
}

static Window selected(void) {
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *data = NULL;
    Window win = None;
    XGetWindowProperty(d, root, active_window, 0, 1, False, XA_WINDOW,
                       &type, &format, &count, &remaining, &data);
    if (format == 32 && count == 1) win = *(unsigned long *)data;
    if (data) XFree(data);
    return win;
}

static void pump(void) {
    XSync(d, False);
    while (XPending(d)) {
        XEvent e;
        XNextEvent(d, &e);
        if (e.type == MotionNotify) motion_time = e.xmotion.time;
        if (e.type == KeyPress || e.type == KeyRelease) {
            for (int i = 0; i < 4; i++) {
                if (e.xkey.keycode != keys[i]) continue;
                if (e.xkey.window != key_target) wrong_target++;
                if ((e.xkey.state & (ShiftMask | LockMask)) != modifiers) wrong_modifiers++;
                if (e.type == KeyPress) presses[i]++;
                else releases[i]++;
            }
        }
        if (e.type == GenericEvent && e.xcookie.extension == xi_opcode && XGetEventData(d, &e.xcookie)) {
            XIRawEvent *raw = e.xcookie.data;
            if (e.xcookie.evtype == XI_RawMotion) raw_motion++;
            if (e.xcookie.evtype == XI_RawKeyPress || e.xcookie.evtype == XI_RawKeyRelease) {
                for (int i = 0; i < 4; i++) {
                    if (raw->detail != keys[i]) continue;
                    if (e.xcookie.evtype == XI_RawKeyPress) raw_presses[i]++;
                    else raw_releases[i]++;
                }
            }
            XFreeEventData(d, &e.xcookie);
        }
        if (e.type == ClientMessage && e.xclient.message_type == protocols &&
            (Atom)e.xclient.data.l[0] == take_focus) {
            offers++;
            offer_time = (Time)e.xclient.data.l[1];
            offer_window = e.xclient.window;
            if (!offer_time) invalid_offers++;
            if (answer_offers && offer_time) {
                XSetInputFocus(d, e.xclient.window, RevertToParent, offer_time);
                XFlush(d);
            }
        }
    }
}

static Time server_time(void) {
    XEvent e;
    unsigned char value = 0;
    while (XCheckTypedWindowEvent(d, clock_window, PropertyNotify, &e)) {}
    XChangeProperty(d, clock_window, clock_atom, XA_INTEGER, 8, PropModeReplace, &value, 1);
    XWindowEvent(d, clock_window, PropertyChangeMask, &e);
    return e.xproperty.time;
}

static void wait_focus(Window win) {
    double deadline = now() + 5;
    do {
        pump();
        if (focused() == win && selected() == win) return;
        pause_events();
    } while (now() < deadline);
    fprintf(stderr, "focus=%lx selected=%lx expected=%lx offers=%u invalid=%u\n",
            focused(), selected(), win, offers, invalid_offers);
    check(0, "real and logical focus converge");
}

static void stable_focus(Window win, double seconds) {
    double deadline = now() + seconds;
    do {
        pump();
        if (focused() != win) check(0, "focus remains on its owner");
        pause_events();
    } while (now() < deadline);
}

static Window window(int input, int protocol) {
    Window win = XCreateSimpleWindow(d, root, 50, 50, 600, 400, 0, 0, 0xffffff);
    XWMHints hints = {0};
    hints.flags = InputHint;
    hints.input = input;
    XSetWMHints(d, win, &hints);
    if (protocol) XSetWMProtocols(d, win, &take_focus, 1);
    XSelectInput(d, win, FocusChangeMask | StructureNotifyMask | KeyPressMask | KeyReleaseMask | PointerMotionMask);
    XStoreName(d, win, "input-probe");
    return win;
}

static void activate(Window win) {
    pump();
    unsigned before = offers;
    XEvent e = {0};
    e.xclient.type = ClientMessage;
    e.xclient.window = win;
    e.xclient.message_type = active_window;
    e.xclient.format = 32;
    e.xclient.data.l[0] = 1;
    e.xclient.data.l[1] = server_time();
    XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
    XFlush(d);
    wait_focus(win);
    Atom *list = NULL;
    int count = 0, takes_focus = 0;
    if (XGetWMProtocols(d, win, &list, &count)) {
        for (int i = 0; i < count; i++) if (list[i] == take_focus) takes_focus = 1;
        XFree(list);
    }
    if (takes_focus) {
        double deadline = now() + 5;
        while ((offers == before || offer_window != win) && now() < deadline) { pump(); pause_events(); }
        check(offers > before && offer_window == win, "explicit activation completes its protocol offer");
    }
}

static void protocol_test(void) {
    Time before = server_time();
    answer_offers = 1;
    Window global = window(False, True);
    XMapWindow(d, global);
    double deadline = now() + 5;
    while (!offers && now() < deadline) { pump(); pause_events(); }
    check(offers && !invalid_offers, "startup WM_TAKE_FOCUS has a nonzero timestamp");
    check((int32_t)((uint32_t)offer_time - (uint32_t)before) >= 0,
          "startup offer uses a current server timestamp without prior input");
    wait_focus(global);
    unsigned initial = offers;
    stable_focus(global, 0.1);
    check(offers == initial, "accepting an offer produces no duplicate WM_TAKE_FOCUS");

    Window passive = window(True, False);
    XMapWindow(d, passive);
    wait_focus(passive);
    activate(global);
    check(!invalid_offers, "focus changes never generate CurrentTime protocol messages");

    before = server_time();
    XSetInputFocus(d, root, RevertToParent, before);
    XFlush(d);
    wait_focus(global);
    check(!invalid_offers && (int32_t)((uint32_t)offer_time - (uint32_t)before) >= 0,
          "repair after external focus loss obtains a fresh protocol timestamp");

    Window local = window(True, True);
    XMapWindow(d, local);
    wait_focus(local);
    deadline = now() + 5;
    initial = offers;
    activate(global);
    while (offers == initial && now() < deadline) { pump(); pause_events(); }
    check(!invalid_offers, "Locally Active and Globally Active offers share valid timestamps");
    initial = offers;
    XEvent old = {0};
    old.xfocus.type = FocusIn;
    old.xfocus.window = local;
    old.xfocus.mode = NotifyNormal;
    old.xfocus.detail = NotifyNonlinear;
    XSendEvent(d, local, False, FocusChangeMask, &old);
    XFlush(d);
    stable_focus(global, 0.1);
    check(offers == initial, "an obsolete FocusIn produces no repair or duplicate protocol offer");
    XDestroyWindow(d, local);
    XDestroyWindow(d, passive);
    XDestroyWindow(d, global);
}

static void transfer(Window to) {
    XSetInputFocus(d, to, RevertToParent, server_time());
    XFlush(d);
    wait_focus(to);
    stable_focus(to, 0.05);
}

static void view(unsigned desktop) {
    Atom current = XInternAtom(d, "_NET_CURRENT_DESKTOP", False);
    XEvent e = {0};
    e.xclient.type = ClientMessage;
    e.xclient.window = root;
    e.xclient.message_type = current;
    e.xclient.format = 32;
    e.xclient.data.l[0] = desktop;
    XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
    XFlush(d);
    double deadline = now() + 5;
    do {
        Atom type;
        int format;
        unsigned long count, left;
        unsigned char *data = NULL;
        XGetWindowProperty(d, root, current, 0, 1, False, XA_CARDINAL, &type, &format, &count, &left, &data);
        int done = format == 32 && count == 1 && *(unsigned long *)data == desktop;
        if (data) XFree(data);
        if (done) return;
        pump(); pause_events();
    } while (now() < deadline);
    check(0, "View switch completes");
}

static void ownership_test(void) {
    Window game = window(True, False);
    XStoreName(d, game, "input-owner");
    XMapWindow(d, game);
    wait_focus(game);
    Window dialog = window(True, True);
    XSetTransientForHint(d, dialog, game);
    XMapWindow(d, dialog);
    wait_focus(dialog);
    activate(game);
    pump();
    unsigned before = offers;
    for (int i = 0; i < 5; i++) { transfer(dialog); transfer(game); }
    check(offers == before, "transient transfers update selection without WM_TAKE_FOCUS or repairs");

    Window nested = window(True, False);
    XSetTransientForHint(d, nested, dialog);
    XMapWindow(d, nested);
    wait_focus(nested);
    transfer(game);
    transfer(nested);
    transfer(dialog);
    check(offers == before, "nested and sibling transient focus remains within the application family");

    activate(game);
    XDeleteProperty(d, dialog, XA_WM_TRANSIENT_FOR);
    XSetInputFocus(d, dialog, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    stable_focus(game, 0.1);
    check(selected() == game, "removing WM_TRANSIENT_FOR revokes the old focus relationship");
    XSetTransientForHint(d, dialog, game);
    transfer(dialog);
    transfer(game);

    Window sibling = window(True, False);
    XWMHints hints = {0};
    hints.flags = InputHint | WindowGroupHint;
    hints.input = True;
    hints.window_group = game;
    XSetWMHints(d, game, &hints);
    XSetWMHints(d, sibling, &hints);
    XMapWindow(d, sibling);
    wait_focus(sibling);
    transfer(game);
    transfer(sibling);
    check(offers == before, "WM_HINTS window groups authorize related top-level transfers");

    hints.flags = InputHint;
    XSetWMHints(d, sibling, &hints);
    Atom leader = XInternAtom(d, "WM_CLIENT_LEADER", False);
    XChangeProperty(d, game, leader, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&game, 1);
    XChangeProperty(d, sibling, leader, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&game, 1);
    transfer(game);
    transfer(sibling);
    check(offers == before, "WM_CLIENT_LEADER supports clients without a window group hint");
    fullscreen(game, 1);
    activate(game);
    transfer(dialog);
    transfer(game);
    XSetInputFocus(d, sibling, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    stable_focus(game, 0.1);
    check(selected() == game, "fullscreen permits owned transients but not unrelated group peers");
    fullscreen(game, 0);

    Window stranger = window(True, True);
    XMapWindow(d, stranger);
    wait_focus(stranger);
    activate(game);
    pump();
    before = offers;
    XSetInputFocus(d, stranger, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    stable_focus(game, 0.2);
    check(selected() == game && offers == before, "an unrelated managed window cannot take the logical selection");

    Window no_input = window(False, False);
    hints.flags = InputHint | WindowGroupHint;
    hints.input = False;
    hints.window_group = game;
    XSetWMHints(d, no_input, &hints);
    XMapWindow(d, no_input);
    XFlush(d);
    double deadline = now() + 5;
    XWindowAttributes attrs;
    do {
        pump();
        XGetWindowAttributes(d, no_input, &attrs);
        if (attrs.map_state == IsViewable) break;
        pause_events();
    } while (now() < deadline);
    activate(game);
    XSetInputFocus(d, no_input, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    stable_focus(game, 0.1);
    check(selected() == game, "No Input clients cannot become the selection through self-focus");
    XDeleteProperty(d, no_input, XA_WM_HINTS);
    activate(no_input);
    check(focused() == no_input, "deleting WM_HINTS restores default input eligibility");
    view(1);
    Window hidden = window(True, False);
    hints.flags = InputHint | WindowGroupHint;
    hints.input = True;
    hints.window_group = game;
    XSetWMHints(d, hidden, &hints);
    XMapWindow(d, hidden);
    wait_focus(hidden);
    view(0);
    activate(game);
    XSetInputFocus(d, hidden, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    stable_focus(game, 0.1);
    check(selected() == game, "a shared group cannot authorize focus on a hidden View");

    XEvent old = {0};
    old.xfocus.type = FocusIn;
    old.xfocus.window = dialog;
    old.xfocus.mode = NotifyNormal;
    old.xfocus.detail = NotifyNonlinear;
    XSendEvent(d, dialog, False, FocusChangeMask, &old);
    XFlush(d);
    stable_focus(game, 0.1);
    check(selected() == game, "an obsolete FocusIn cannot undo the current WM selection");
    XDestroyWindow(d, hidden);
    XDestroyWindow(d, no_input);
    XDestroyWindow(d, stranger);
    XDestroyWindow(d, sibling);
    XDestroyWindow(d, nested);
    XDestroyWindow(d, dialog);
    XDestroyWindow(d, game);
}

static void input_setup(void) {
    KeySym symbols[] = {XK_w, XK_a, XK_s, XK_d};
    for (int i = 0; i < 4; i++) keys[i] = XKeysymToKeycode(d, symbols[i]);
    Bool supported;
    XkbSetDetectableAutoRepeat(d, True, &supported);
    check(supported, "client supports detectable autorepeat without changing server repeat settings");
    int event, error, major = 2, minor = 2;
    check(XQueryExtension(d, "XInputExtension", &xi_opcode, &event, &error)
          && XIQueryVersion(d, &major, &minor) == Success, "XI2 raw input is available");
    unsigned char bits[XIMaskLen(XI_RawMotion)] = {0};
    XISetMask(bits, XI_RawKeyPress);
    XISetMask(bits, XI_RawKeyRelease);
    XISetMask(bits, XI_RawMotion);
    XIEventMask mask = {XIAllMasterDevices, sizeof bits, bits};
    XISelectEvents(d, root, &mask, 1);
    XkbLockModifiers(d, XkbUseCoreKbd, LockMask, 0);
    pump();
}

static void child_focus_test(void) {
    Window game = window(True, False);
    XMapWindow(d, game);
    wait_focus(game);
    XWarpPointer(d, None, game, 0, 0, 0, 0, 20, 20);
    XFlush(d);
    double deadline = now() + 5;
    while (!motion_time && now() < deadline) { pump(); pause_events(); }
    check(motion_time != 0, "WM receives input before an internal focus transition");
    Time stamp;
    do { stamp = server_time(); pause_events(); }
    while ((int32_t)((uint32_t)stamp - (uint32_t)motion_time) <= 0 && now() < deadline);
    check((int32_t)((uint32_t)stamp - (uint32_t)motion_time) > 0,
          "child focus changes use a timestamp newer than the WM's cached input");
    Window child = XCreateSimpleWindow(d, game, 10, 10, 100, 100, 0, 0, 0);
    XMapWindow(d, child);
    XSetInputFocus(d, child, RevertToParent, stamp);
    XSync(d, False);
    XSetInputFocus(d, game, RevertToParent, server_time());
    XSync(d, False);
    Window other = window(True, False);
    XMapWindow(d, other);
    wait_focus(other);
    check(focused() == other && selected() == other, "internal child focus cannot leave the next WM request silently stale");
    XDestroyWindow(d, other);
    XDestroyWindow(d, game);
}

static void key_reset(void) {
    pump();
    memset(presses, 0, sizeof presses);
    memset(releases, 0, sizeof releases);
    memset(raw_presses, 0, sizeof raw_presses);
    memset(raw_releases, 0, sizeof raw_releases);
    wrong_target = wrong_modifiers = 0;
}

static void held_keys(unsigned mask, double seconds) {
    key_reset();
    for (int i = 0; i < 4; i++) if (mask & (1u << i)) XTestFakeKeyEvent(d, keys[i], True, 0);
    XFlush(d);
    double deadline = now() + seconds;
    do { pump(); pause_events(); } while (now() < deadline);
    if (seconds >= 0.8)
        printf("REPEAT mask=%u core=%u,%u,%u,%u raw=%u,%u,%u,%u\n", mask,
               presses[0], presses[1], presses[2], presses[3],
               raw_presses[0], raw_presses[1], raw_presses[2], raw_presses[3]);
    for (int i = 0; i < 4; i++) {
        if (mask & (1u << i)) {
            check(presses[i] > 0 && releases[i] == 0, "held key stays pressed in the client");
            check(raw_presses[i] > 0 && raw_releases[i] == 0, "XI2 preserves held key state");
        } else {
            check(!presses[i] && !releases[i], "unpressed keys generate no client events");
        }
    }
    if (seconds >= 0.8) {
        // XKB repeats the last pressed key; XI2 raw tracks physical transitions.
        int latest = 0;
        for (int i = 0; i < 4; i++) if (mask & (1u << i)) latest = i;
        check(presses[latest] > 1, "held combinations receive core autorepeat without release");
    }
    check(!wrong_target && !wrong_modifiers, "key destination and Shift/Caps modifiers match");
    for (int i = 0; i < 4; i++) if (mask & (1u << i)) XTestFakeKeyEvent(d, keys[i], False, 0);
    XFlush(d);
    deadline = now() + 2;
    unsigned done;
    do {
        pump();
        done = 0;
        for (int i = 0; i < 4; i++) if (releases[i] == 1 && raw_releases[i] == 1) done |= 1u << i;
        if (done == mask) break;
        pause_events();
    } while (now() < deadline);
    check(done == mask && !wrong_target, "each held key has exactly one core and XI2 release on its owner");
    char physical[32];
    XQueryKeymap(d, physical);
    for (int i = 0; i < 4; i++) check(!(physical[keys[i] / 8] & (1 << (keys[i] % 8))), "no movement key remains stuck");
}

static Window clipping_window(void) {
    XSetWindowAttributes attrs = {0};
    attrs.override_redirect = True;
    Window clip = XCreateWindow(d, root, 200, 150, 300, 250, 0, CopyFromParent,
                                InputOnly, CopyFromParent, CWOverrideRedirect, &attrs);
    XSelectInput(d, clip, StructureNotifyMask | FocusChangeMask | PointerMotionMask | ButtonPressMask | ButtonReleaseMask | KeyPressMask | KeyReleaseMask);
    XMapWindow(d, clip);
    XSync(d, False);
    return clip;
}

static void confine(Window clip) {
    check(XGrabPointer(d, clip, False, PointerMotionMask | ButtonPressMask | ButtonReleaseMask,
                       GrabModeAsync, GrabModeAsync, clip, None, server_time()) == GrabSuccess,
          "InputOnly override-redirect pointer grab succeeds");
    const int directions[][2] = {{2000, 0}, {-2000, 0}, {0, 2000}, {0, -2000}};
    for (unsigned i = 0; i < sizeof directions / sizeof directions[0]; i++) {
        unsigned initial = raw_motion;
        XTestFakeRelativeMotionEvent(d, directions[i][0], directions[i][1], 0);
        XFlush(d);
        double deadline = now() + 2;
        do { pump(); pause_events(); } while (raw_motion == initial && now() < deadline);
        Window r, child;
        int rx, ry, wx, wy;
        unsigned state;
        XQueryPointer(d, root, &r, &child, &rx, &ry, &wx, &wy, &state);
        check(rx >= 200 && rx < 500 && ry >= 150 && ry < 400, "pointer remains within the clip rectangle at screen boundaries");
        check(raw_motion > initial, "XI2 raw motion remains available during confinement");
    }
}

static void fullscreen(Window win, int enabled) {
    XEvent e = {0};
    e.xclient.type = ClientMessage;
    e.xclient.window = win;
    e.xclient.message_type = XInternAtom(d, "_NET_WM_STATE", False);
    e.xclient.format = 32;
    e.xclient.data.l[0] = enabled;
    e.xclient.data.l[1] = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
    e.xclient.data.l[3] = 1;
    XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
    XFlush(d);
    double deadline = now() + 5;
    do {
        Atom type;
        int format;
        unsigned long count, left;
        unsigned char *data = NULL;
        XGetWindowProperty(d, win, e.xclient.message_type, 0, 32, False, XA_ATOM, &type, &format, &count, &left, &data);
        int found = 0;
        if (format == 32) for (unsigned long i = 0; i < count; i++) if (((Atom *)data)[i] == (Atom)e.xclient.data.l[1]) found = 1;
        if (data) XFree(data);
        if (found == enabled) { pump(); return; }
        pause_events();
    } while (now() < deadline);
    check(0, "fullscreen state converges");
}

static void capture_test(void) {
    input_setup();
    answer_offers = 1;
    Window game = window(True, True);
    XStoreName(d, game, "captured-game");
    XMapWindow(d, game);
    wait_focus(game);
    key_target = game;
    for (int i = 0; i < 4; i++) held_keys(1u << i, 0.03);
    for (int i = 1; i < 4; i++) held_keys(1u | (1u << i), 0.85);
    modifiers = ShiftMask;
    KeyCode shift = XKeysymToKeycode(d, XK_Shift_L);
    XTestFakeKeyEvent(d, shift, True, 0);
    held_keys(15, 0.1);
    XTestFakeKeyEvent(d, shift, False, 0);
    modifiers = LockMask;
    XkbLockModifiers(d, XkbUseCoreKbd, LockMask, LockMask);
    held_keys(15, 0.1);
    modifiers = ShiftMask | LockMask;
    XTestFakeKeyEvent(d, shift, True, 0);
    held_keys(15, 0.1);
    XTestFakeKeyEvent(d, shift, False, 0);
    XkbLockModifiers(d, XkbUseCoreKbd, LockMask, 0);
    modifiers = 0;

    Window clip = clipping_window();
    for (int mode = 0; mode < 3; mode++) {
        fullscreen(game, mode == 2);
        if (mode == 1) {
            XSetWindowBorderWidth(d, game, 0);
            activate(game);
            XWindowAttributes attrs;
            double deadline = now() + 5;
            do { XGetWindowAttributes(d, game, &attrs); pump(); pause_events(); }
            while (attrs.border_width != 0 && now() < deadline);
            check(attrs.border_width == 0, "borderless mode actually removes the client border");
        }
        confine(clip);
        unsigned before = offers;
        held_keys(15, 0.1);
        stable_focus(game, 0.05);
        check(offers == before, "capture and mode changes produce no spurious focus offers");
        XUngrabPointer(d, server_time());
        XSync(d, False);
    }
    fullscreen(game, 0);

    Window child = XCreateSimpleWindow(d, game, 10, 10, 100, 100, 0, 0, 0);
    XSelectInput(d, child, FocusChangeMask | KeyPressMask | KeyReleaseMask);
    XMapWindow(d, child);
    key_reset();
    XTestFakeKeyEvent(d, keys[0], True, 0);
    XFlush(d);
    double press_deadline = now() + 2;
    while (!presses[0] && now() < press_deadline) { pump(); pause_events(); }
    check(presses[0] == 1 && !wrong_target, "held movement begins on the current owner before transfer");
    XSetInputFocus(d, child, RevertToParent, server_time());
    XFlush(d);
    double deadline = now() + 2;
    do { pump(); pause_events(); } while (focused() != child && now() < deadline);
    key_target = child;
    char physical[32];
    XQueryKeymap(d, physical);
    check(physical[keys[0] / 8] & (1 << (keys[0] % 8)), "an intentional child transfer does not release the physical held key");
    XTestFakeKeyEvent(d, keys[0], False, 0);
    XFlush(d);
    double release_deadline = now() + 2;
    while (!releases[0] && now() < release_deadline) { pump(); pause_events(); }
    check(releases[0] == 1 && !wrong_target, "release follows the explicitly chosen child without a WM counter-transfer");
    unsigned before = offers;
    held_keys(15, 0.1);
    check(focused() == child && selected() == game && offers == before, "child-window focus preserves logical owner and keyboard delivery");
    XDestroyWindow(d, child);
    XSetInputFocus(d, game, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    key_target = game;

    XSetWindowAttributes attrs = {0};
    attrs.override_redirect = True;
    Window popup = XCreateWindow(d, root, 100, 100, 200, 150, 0, CopyFromParent,
                                 InputOutput, CopyFromParent, CWOverrideRedirect, &attrs);
    XSelectInput(d, popup, FocusChangeMask | KeyPressMask | KeyReleaseMask);
    XMapWindow(d, popup);
    XSetInputFocus(d, popup, RevertToParent, server_time());
    XSync(d, False);
    before = offers;
    key_target = popup;
    held_keys(15, 0.05);
    stable_focus(popup, 0.05);
    check(selected() == game && offers == before, "unmanaged popup owns input without changing logical selection");
    XDestroyWindow(d, popup);
    XSetInputFocus(d, game, RevertToParent, server_time());
    XFlush(d);
    wait_focus(game);
    key_target = game;

    confine(clip);
    unsigned captured = offers;
    check(XGrabKeyboard(d, clip, False, GrabModeAsync, GrabModeAsync, server_time()) == GrabSuccess,
          "client keyboard grab succeeds without changing logical focus");
    key_target = clip;
    held_keys(15, 0.05);
    check(selected() == game && offers == captured, "NotifyGrab leaves selection and focus protocol quiet");
    XUngrabKeyboard(d, server_time());
    XUngrabPointer(d, server_time());
    pump();
    key_target = game;
    held_keys(15, 0.05);
    check(selected() == game && offers == captured, "NotifyUngrab restores normal keyboard delivery without repair");

    Window other = window(True, False);
    XMapWindow(d, other);
    wait_focus(other);
    activate(game);
    for (int i = 0; i < 3; i++) {
        confine(clip);
        held_keys(3, 0.05);
        activate(other);
        XUngrabPointer(d, server_time());
        activate(game);
        confine(clip);
        held_keys(12, 0.05);
        XUngrabPointer(d, server_time());
    }
    check(!invalid_offers, "focus release and reacquisition retain valid protocol timestamps");
    XDestroyWindow(d, other);
    XDestroyWindow(d, clip);
    XDestroyWindow(d, game);
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    setvbuf(stdout, NULL, _IOLBF, 0);
    d = XOpenDisplay(NULL);
    if (!d) return 2;
    root = DefaultRootWindow(d);
    protocols = XInternAtom(d, "WM_PROTOCOLS", False);
    take_focus = XInternAtom(d, "WM_TAKE_FOCUS", False);
    active_window = XInternAtom(d, "_NET_ACTIVE_WINDOW", False);
    clock_atom = XInternAtom(d, "_INPUT_PROBE_TIME", False);
    clock_window = XCreateSimpleWindow(d, root, 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(d, clock_window, PropertyChangeMask);
    if (!strcmp(argv[1], "protocol")) protocol_test();
    else if (!strcmp(argv[1], "ownership")) ownership_test();
    else if (!strcmp(argv[1], "capture")) capture_test();
    else if (!strcmp(argv[1], "child")) child_focus_test();
    else return 2;
    XCloseDisplay(d);
    return 0;
}
