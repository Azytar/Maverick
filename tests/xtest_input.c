// Synthetic XTEST input sender for the restart-stress harness.
//
// Hammers the nested display with fake key presses, pointer motion and button
// clicks via the XTEST extension, so the window manager's input path (XKB
// keymap refresh, keygrab dispatch, pointer grabs) is under load while
// `maverickctl restart` re-execs it. No XTEST client exists in tests/ yet
// (stress.c churns *windows*, not input), and xdotool/xte are unavailable in
// this environment.
//
// Usage: xtest_input [DURATION_SECONDS] [DISPLAY]
// Prints "XTEST_INPUT_DONE" on stdout when it exits; the harness treats the
// absence of that marker (kill -9 by timeout) as an input-path hang.
//
// Events are legal but arbitrary: modifier toggles (so keymap refresh paths
// run), motion across the whole screen, and clicks at varying positions. None
// of them need to *do* anything useful — the point is volume during restart.

#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    double duration = (argc >= 2) ? atof(argv[1]) : 5.0;
    if (duration <= 0) duration = 5.0;
    if (argc >= 3) setenv("DISPLAY", argv[2], 1);

    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "xtest_input: cannot open display\n"); return 1; }

    int event_base, error_base, major, minor;
    if (!XTestQueryExtension(dpy, &event_base, &error_base, &major, &minor)) {
        fprintf(stderr, "xtest_input: XTEST extension not available\n");
        return 1;
    }
    fprintf(stderr, "xtest_input: XTEST %d.%d firing for %.1fs\n", major, minor, duration);

    int screen = DefaultScreen(dpy);
    int w = DisplayWidth(dpy, screen), h = DisplayHeight(dpy, screen);

    // Keysyms the compiled config binds (Mod4 combos) plus plain modifiers.
    long keysyms[] = {
        XK_space, XK_Return, XK_Tab, XK_Left, XK_Right, XK_Up, XK_Down,
        XK_plus, XK_bracketleft, XK_bracketright, XK_f, XK_m, XK_q, XK_r,
        XK_Shift_L, XK_Control_L, XK_Alt_L, XK_Super_L,
    };
    int nkeys = (int)(sizeof(keysyms) / sizeof(keysyms[0]));

    srand((unsigned)(now_s() * 1000000));
    double deadline = now_s() + duration;
    long nkeys_sent = 0, nmotion = 0, nclick = 0;

    while (now_s() < deadline) {
        int ksi = rand() % nkeys;
        KeySym ks = keysyms[ksi];
        KeyCode kc = XKeysymToKeycode(dpy, ks);
        if (kc == 0) continue;

        // press + release (some repeats so autorepeat paths run too)
        XTestFakeKeyEvent(dpy, kc, True, 0);
        XTestFakeKeyEvent(dpy, kc, False, 0);
        nkeys_sent++;

        // pointer motion, coarse sweep + jitter
        int mx = rand() % w, my = rand() % h;
        XTestFakeMotionEvent(dpy, screen, mx, my, 0);
        nmotion++;
        if (rand() % 3 == 0) {
            XTestFakeMotionEvent(dpy, screen,
                                 mx + (rand() % 101) - 50,
                                 my + (rand() % 101) - 50, 0);
            nmotion++;
        }
        // button click, varying button
        if (rand() % 4 == 0) {
            XTestFakeButtonEvent(dpy, 1 + (rand() % 3), True, 0);
            XTestFakeButtonEvent(dpy, 1 + (rand() % 3), False, 0);
            nclick++;
        }
        XFlush(dpy);
        usleep(2000); // ~500 events/s aggregate — noticeable load, not a flood
    }

    printf("XTEST_INPUT_DONE keys=%ld motion=%ld click=%ld\n",
           nkeys_sent, nmotion, nclick);
    fflush(stdout);
    XCloseDisplay(dpy);
    return 0;
}
