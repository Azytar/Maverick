#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
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
static int answer_offers;

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
        if (e.type == ClientMessage && e.xclient.message_type == protocols &&
            (Atom)e.xclient.data.l[0] == take_focus) {
            offers++;
            offer_time = (Time)e.xclient.data.l[1];
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
    XSelectInput(d, win, FocusChangeMask | StructureNotifyMask | KeyPressMask | KeyReleaseMask);
    XStoreName(d, win, "input-probe");
    return win;
}

static void activate(Window win) {
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

static void ownership_test(void) {
    Window game = window(True, False);
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
    XDestroyWindow(d, no_input);
    XDestroyWindow(d, stranger);
    XDestroyWindow(d, sibling);
    XDestroyWindow(d, nested);
    XDestroyWindow(d, dialog);
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
    else return 2;
    XCloseDisplay(d);
    return 0;
}
