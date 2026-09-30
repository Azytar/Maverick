// XTEST input-saturation stress sender for Maverick (test-only, Agent B).
//
// Sends synthetic pointer/keyboard input at a controlled rate so the WM's
// input pipeline can be driven past its steady-state capacity and the
// backlog behaviour measured (see tests/saturation.py).
//
// Usage: xtest-stress MODE RATE DURATION [X Y]
//
//   MODE   scroll | click | motion | combined | maxscroll | maxclick | max
//   RATE   events (notches/clicks) per second for the paced modes.
//          0 means "unpaced" (fire as fast as the server accepts) for any
//          mode; the max* modes are unpaced synonyms.
//   DURATION  seconds. ALWAYS argv[3] -- for every mode, including max*.
//
//   scroll    RATE notches/s, Mod4 held, button-5 press+release
//   click     RATE clicks/s, button-1 press+release
//   motion    RATE motion events/s, small jitter around (X,Y)
//   combined  RATE pairs/s, alternating scroll notch / click, Mod4 held
//   maxscroll unpaced scroll notches (alias: max)
//   maxclick  unpaced clicks
//
//   X Y is the pointer position to target (default 640,360 -- over a client).
//
// Build: cc -O2 -o xtest-stress xtest-stress.c -lX11 -lXtst
//
// Output: one STRESS_RESULT line + STRESS_DONE on stdout.
// With XTEST_STRESS_INJECT=<path>, an injection log is also written:
//   INJPROG <monotonic_ns> <cumulative_sent>
// one line every 20 ms, plus
//   INJ <monotonic_ns> <kind> <detail>
// for every event when the effective rate is <= 2000/s (too much I/O above
// that; the INJPROG series still reconstructs the arrival curve).

#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static Display *dpy;
static int super_code;
static FILE *inj;
static double last_progress = 0;

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static void sleep_until(double t) {
    struct timespec ts;
    ts.tv_sec = (time_t)t;
    ts.tv_nsec = (long)((t - ts.tv_sec) * 1e9);
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL);
}

static void progress(long sent) {
    if (!inj) return;
    double t = now_s();
    if (t - last_progress < 0.02) return;
    last_progress = t;
    fprintf(inj, "INJPROG %lld %ld\n", now_ns(), sent);
}

static void mark(const char *kind, int detail) {
    if (!inj) return;
    fprintf(inj, "INJ %lld %s %d\n", now_ns(), kind, detail);
}

static void move_pointer(int x, int y) {
    XTestFakeMotionEvent(dpy, -1, x, y, CurrentTime);
    XFlush(dpy);
    // Give the server a moment to process the motion before the next event, so
    // the server does not coalesce them (X11 merges bursts of MotionNotify into
    // a single event carrying the final position).
    usleep(200);
}

static void key(unsigned code, int down) {
    XTestFakeKeyEvent(dpy, code, down, CurrentTime);
}

static void button(unsigned detail, int down) {
    XTestFakeButtonEvent(dpy, detail, down, CurrentTime);
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr,
                "usage: %s MODE RATE DURATION [X Y]\n"
                "  MODE = scroll | click | motion | combined | maxscroll | maxclick | max\n"
                "  RATE = events/s for paced modes (0 = unpaced); ignored by max*\n"
                "  DURATION = seconds (always argv[3])\n"
                "  max is an alias for maxscroll\n",
                argv[0]);
        return 2;
    }

    const char *mode = argv[1];
    const double rate = atof(argv[2]);
    const double duration = atof(argv[3]);
    const int px = (argc >= 6) ? atoi(argv[4]) : 640;
    const int py = (argc >= 6) ? atoi(argv[5]) : 360;

    if (duration <= 0) { fprintf(stderr, "xtest-stress: bad duration\n"); return 2; }

    const int unpaced = !strcmp(mode, "max") || !strcmp(mode, "maxscroll") ||
                        !strcmp(mode, "maxclick") || rate <= 0;
    // Which button the unpaced flood uses.
    const unsigned flood_button = !strcmp(mode, "maxclick") ? 1u : 5u;

    const char *inj_path = getenv("XTEST_STRESS_INJECT");
    if (inj_path && *inj_path) {
        inj = fopen(inj_path, "w");
        if (!inj) fprintf(stderr, "xtest-stress: cannot open inject log %s\n", inj_path);
    }
    const int log_every = rate <= 2000 && !unpaced;

    dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "xtest-stress: cannot open display\n"); return 1; }

    const KeySym ks = XK_Super_L;
    super_code = XKeysymToKeycode(dpy, ks);
    if (super_code == 0) { fprintf(stderr, "xtest-stress: no Super_L keycode\n"); return 1; }

    // Make sure no modifiers are held at start (release Super defensively).
    key(super_code, False);
    XSync(dpy, False);

    move_pointer(px, py);

    const double t0 = now_s();
    const double deadline = t0 + duration;
    long sent = 0, notches = 0, clicks = 0, motions = 0;
    const int hold_mod4 = !strcmp(mode, "scroll") || !strcmp(mode, "combined") ||
                          flood_button == 5;
    const double interval = (!unpaced && rate > 0) ? 1.0 / rate : 0;

    if (hold_mod4) { key(super_code, True); XFlush(dpy); }

    if (unpaced) {
        while (now_s() < deadline) {
            button(flood_button, True);
            button(flood_button, False);
            XFlush(dpy);
            sent++;
            if (flood_button == 5) notches++; else clicks++;
            if (log_every) mark("button", (int)flood_button);
            progress(sent);
        }
    } else if (!strcmp(mode, "scroll")) {
        double t = t0;
        while (t < deadline) {
            button(5, True);
            button(5, False);
            XFlush(dpy);
            sent++; notches++;
            if (log_every) mark("button", 5);
            progress(sent);
            t += interval;
            if (interval > 0) sleep_until(t);
        }
    } else if (!strcmp(mode, "click")) {
        double t = t0;
        while (t < deadline) {
            button(1, True);
            button(1, False);
            XFlush(dpy);
            sent++; clicks++;
            if (log_every) mark("button", 1);
            progress(sent);
            t += interval;
            if (interval > 0) sleep_until(t);
        }
    } else if (!strcmp(mode, "motion")) {
        double t = t0;
        while (t < deadline) {
            move_pointer(px + (sent % 7) - 3, py + ((sent / 7) % 5) - 2);
            sent++; motions++;
            if (log_every) mark("motion", 0);
            progress(sent);
            t += interval;
            if (interval > 0) sleep_until(t);
        }
    } else if (!strcmp(mode, "combined")) {
        double t = t0;
        int flip = 0;
        while (t < deadline) {
            if (flip & 1) {
                button(1, True); button(1, False); clicks++;
            } else {
                button(5, True); button(5, False); notches++;
            }
            XFlush(dpy);
            sent++;
            if (log_every) mark("button", (flip & 1) ? 1 : 5);
            progress(sent);
            flip++;
            t += interval;
            if (interval > 0) sleep_until(t);
        }
    } else {
        fprintf(stderr, "xtest-stress: unknown mode '%s'\n", mode);
        return 2;
    }

    if (hold_mod4) { key(super_code, False); XFlush(dpy); }
    if (inj) { fprintf(inj, "INJPROG %lld %ld\n", now_ns(), sent); fflush(inj); }

    const double elapsed = now_s() - t0;
    printf("STRESS_RESULT mode=%s sent=%ld notches=%ld clicks=%ld motions=%ld "
           "elapsed=%.3f rate_sent=%.1f requested_rate=%.1f\n",
           mode, sent, notches, clicks, motions, elapsed,
           elapsed > 0 ? sent / elapsed : 0, rate);
    printf("STRESS_DONE\n");
    fflush(stdout);
    if (inj) fclose(inj);
    XCloseDisplay(dpy);
    return 0;
}
