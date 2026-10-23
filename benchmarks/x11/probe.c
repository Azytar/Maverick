#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* The same passive clients and EWMH requests run under every WM. Timings end
 * at observable X state, not at a CLI dispatch receipt or a drawing frame. */
static Display *d;
static Window root, windows[256];
static unsigned count;
static Atom clients, active, state, fullscreen, supporting, wm_state;
static double samples[10000];
static int normal_x, normal_y, normal_w, normal_h, normal_border;

static void fail(const char *message) {
    fprintf(stderr, "benchmark probe: %s\n", message);
    exit(2);
}
static int xerror(Display *display, XErrorEvent *event) {
    char message[128];
    XGetErrorText(display, event->error_code, message, sizeof(message));
    fprintf(stderr, "benchmark probe: %s request=%u resource=%lx\n",
            message, event->request_code, event->resourceid);
    exit(2);
}
static double now(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) fail("clock_gettime failed");
    return t.tv_sec + t.tv_nsec / 1e9;
}
/* Xlib returns format-32 elements as native unsigned longs on LP64. */
static int property(Window window, Atom key, Atom expected, unsigned long value,
                    int singleton) {
    Atom type;
    int format;
    unsigned long n, after;
    unsigned char *data = NULL;
    int status = XGetWindowProperty(d, window, key, 0, 4096, False,
                                   AnyPropertyType, &type, &format, &n, &after, &data);
    if (status != Success || (type != None &&
        (type != expected || format != 32 || after || (singleton && n != 1))))
        fail("invalid or truncated X property");
    int found = 0;
    if (type != None) {
        if (n && !data) fail("missing property data");
        for (unsigned long i = 0; i < n; i++)
            if (((unsigned long *)data)[i] == value) found = 1;
    }
    if (data) XFree(data);
    return found;
}
static int ready(void) {
    Atom type;
    int format;
    unsigned long n, after;
    unsigned char *data = NULL;
    if (XGetWindowProperty(d, root, supporting, 0, 1, False, XA_WINDOW,
                          &type, &format, &n, &after, &data) != Success)
        fail("readiness query failed");
    Window child = None;
    if (type == XA_WINDOW && format == 32 && n == 1 && !after && data)
        child = *(unsigned long *)data;
    if (data) XFree(data);
    /* EWMH requires a self-reference on the WM's supporting window. */
    return child != None && property(child, supporting, XA_WINDOW, child, 1);
}
static int focused(Window window) {
    Window focus;
    int revert;
    if (!XGetInputFocus(d, &focus, &revert)) fail("focus query failed");
    return focus == window && property(root, active, XA_WINDOW, window, 1);
}
static int mapped(Window window) {
    XWindowAttributes a;
    if (!XGetWindowAttributes(d, window, &a)) fail("attributes query failed");
    return a.map_state == IsViewable && property(root, clients, XA_WINDOW, window, 0)
        && property(window, wm_state, wm_state, NormalState, 0);
}
static int full(Window window, int wanted) {
    int enabled = property(window, state, XA_ATOM, fullscreen, 0);
    XWindowAttributes a;
    Window child;
    int x, y;
    if (!XGetWindowAttributes(d, window, &a) ||
        !XTranslateCoordinates(d, window, root, 0, 0, &x, &y, &child))
        fail("fullscreen geometry query failed");
    if (!wanted) return !enabled && x == normal_x && y == normal_y &&
        a.width == normal_w && a.height == normal_h && a.border_width == normal_border;
    return enabled && a.map_state == IsViewable && x == 0 && y == 0 &&
        a.width == DisplayWidth(d, DefaultScreen(d)) &&
        a.height == DisplayHeight(d, DefaultScreen(d)) && a.border_width == 0;
}
enum condition { READY, MAP, FOCUS, FULL, NORMAL, GONE };
static void wait_state(enum condition kind, Window window) {
    double deadline = now() + (kind == READY ? 30 : 5);
    for (;;) {
        int ok = 0;
        switch (kind) {
            case READY: ok = ready(); break;
            case MAP: ok = mapped(window); break;
            case FOCUS: ok = focused(window); break;
            case FULL: ok = full(window, 1); break;
            case NORMAL: ok = full(window, 0); break;
            case GONE: ok = !property(root, clients, XA_WINDOW, window, 0); break;
        }
        if (ok) return;
        double remaining = deadline - now();
        if (remaining <= 0) {
            fprintf(stderr, "benchmark probe: condition %d window=%lx timed out\n", kind, window);
            if (kind != READY && kind != GONE) {
                XWindowAttributes a;
                XGetWindowAttributes(d, window, &a);
                fprintf(stderr, "map_state=%d managed=%d normal=%d focus=%d\n", a.map_state,
                        property(root, clients, XA_WINDOW, window, 0),
                        property(window, wm_state, wm_state, NormalState, 0), focused(window));
            }
            exit(2);
        }
        /* Query after every event. No fixed settling sleep or 1ms polling
         * floor; synchronous queries and observer CPU are still in the path. */
        if (XPending(d)) {
            XEvent event;
            XNextEvent(d, &event);
        } else {
            struct pollfd fd = {ConnectionNumber(d), POLLIN, 0};
            int result = poll(&fd, 1, (int)(remaining * 1000) + 1);
            if (result < 0 && errno != EINTR) fail("poll failed");
        }
    }
}
static void message(Window window, Atom kind, long first, long second, long third) {
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = window;
    event.xclient.message_type = kind;
    event.xclient.format = 32;
    event.xclient.data.l[0] = first;
    event.xclient.data.l[1] = second;
    event.xclient.data.l[2] = third;
    if (!XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &event))
        fail("XSendEvent failed");
    XFlush(d);
}
static void focus(Window window) {
    message(window, active, 2, CurrentTime, 0);  /* EWMH pager request. */
    wait_state(FOCUS, window);
}
static void output(const char *phase, unsigned n) {
    printf("{\"phase\":\"%s\",\"samples_ms\":[", phase);
    for (unsigned i = 0; i < n; i++) printf("%s%.6f", i ? "," : "", samples[i]);
    puts("]}");
}
static unsigned positive(const char *text, unsigned maximum) {
    char *end;
    errno = 0;
    long n = strtol(text, &end, 10);
    if (errno || !*text || *end || n < 1 || n > maximum) fail("invalid operation count");
    return (unsigned)n;
}
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--describe")) {
        puts("{\"schema\":1,\"commands\":[\"ready\",\"prime\",\"focus-capability\",\"map N\",\"focus N\",\"fullscreen N\",\"destroy\",\"quit\"]}");
        return 0;
    }
    if (argc != 1) fail("unknown option");
    d = XOpenDisplay(NULL);
    if (!d) fail("cannot open private display");
    XSetErrorHandler(xerror);
    root = DefaultRootWindow(d);
    clients = XInternAtom(d, "_NET_CLIENT_LIST", False);
    active = XInternAtom(d, "_NET_ACTIVE_WINDOW", False);
    state = XInternAtom(d, "_NET_WM_STATE", False);
    fullscreen = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
    supporting = XInternAtom(d, "_NET_SUPPORTING_WM_CHECK", False);
    wm_state = XInternAtom(d, "WM_STATE", False);
    XSelectInput(d, root, PropertyChangeMask | SubstructureNotifyMask);
    XSync(d, False);
    setvbuf(stdout, NULL, _IOLBF, 0);
    puts("{\"probe_ready\":true}");
    char line[128];
    while (fgets(line, sizeof(line), stdin)) {
        char command[32], number[32], extra;
        int fields = sscanf(line, "%31s %31s %c", command, number, &extra);
        if (!strchr(line, '\n') || fields < 1) fail("invalid command");
        if (!strcmp(command, "quit") && fields == 1) break;
        if (!strcmp(command, "ready") && fields == 1) {
            wait_state(READY, root);
            puts("{\"wm_ready\":true}");
        } else if (!strcmp(command, "prime") && fields == 1 && count == 0) {
            /* Some WMs advertise EWMH before finishing startup and discard
             * early MapRequests. A retried, untimed client is a usable-loop
             * barrier; never retry or hide failures in measured operations. */
            Window w = XCreateSimpleWindow(d, root, 50, 50, 400, 300, 0, 0, 0);
            XWMHints hints = {0};
            hints.flags = InputHint | StateHint; hints.input = True; hints.initial_state = NormalState;
            XSetWMHints(d, w, &hints);
            XStoreName(d, w, "Benchmark startup barrier");
            double deadline = now() + 30;
            while (!mapped(w)) {
                if (now() >= deadline) fail("usable event-loop barrier timed out");
                XMapWindow(d, w);
                XFlush(d);
                struct timespec pause = {0, 10000000};
                nanosleep(&pause, NULL);
            }
            XDestroyWindow(d, w);
            XFlush(d);
            wait_state(GONE, w);
            puts("{\"primed\":true}");
        } else if (!strcmp(command, "map") && fields == 2 && count == 0) {
            unsigned n = positive(number, 256);
            for (unsigned i = 0; i < n; i++) {
                Window w = XCreateSimpleWindow(d, root, 50, 50, 400, 300, 0, 0, 0xeeeeee);
                XWMHints hints = {0};
                hints.flags = InputHint | StateHint; hints.input = True; hints.initial_state = NormalState;
                XSetWMHints(d, w, &hints);
                XClassHint class_hint = {"bench-client", "BenchClient"};
                XSetClassHint(d, w, &class_hint);
                XStoreName(d, w, "Benchmark passive client");
                XSelectInput(d, w, PropertyChangeMask | StructureNotifyMask | FocusChangeMask);
                XSync(d, False);  /* Creation/property setup is outside map timing. */
                double start = now();
                XMapWindow(d, w);
                XFlush(d);
                wait_state(MAP, w);
                samples[i] = (now() - start) * 1000;
                windows[count++] = w;
            }
            output("map", n);
        } else if (!strcmp(command, "focus-capability") && fields == 1 && count > 1) {
            Window w = windows[0];
            message(w, active, 2, CurrentTime, 0);
            double deadline = now() + .5;
            while (!focused(w) && now() < deadline) {
                struct timespec pause = {0, 10000000};
                nanosleep(&pause, NULL);
            }
            XWMHints *hints = XGetWMHints(d, w);
            int urgent = hints && (hints->flags & XUrgencyHint);
            if (hints) XFree(hints);
            printf("{\"focus_supported\":%s,\"urgent_only\":%s}\n",
                   focused(w) ? "true" : "false", urgent && !focused(w) ? "true" : "false");
        } else if (!strcmp(command, "focus") && fields == 2 && count > 1) {
            unsigned n = positive(number, 10000);
            /* Establish a distinct starting destination outside the timer. */
            focus(windows[count - 1]);
            for (unsigned i = 0; i < n; i++) {
                double start = now();
                focus(windows[i % count]);
                samples[i] = (now() - start) * 1000;
            }
            output("focus", n);
        } else if (!strcmp(command, "fullscreen") && fields == 2 && count) {
            unsigned n = positive(number, 10000);
            if (n % 2) fail("fullscreen count must be even");
            Window w = windows[count - 1];
            if (!focused(w)) focus(w);
            XWindowAttributes normal;
            Window child;
            if (!XGetWindowAttributes(d, w, &normal) ||
                !XTranslateCoordinates(d, w, root, 0, 0, &normal_x, &normal_y, &child))
                fail("normal geometry query failed");
            normal_w = normal.width; normal_h = normal.height; normal_border = normal.border_width;
            for (unsigned i = 0; i < n; i++) {
                int enabled = i % 2 == 0;
                double start = now();
                message(w, state, enabled ? 1 : 0, (long)fullscreen, 0);
                wait_state(enabled ? FULL : NORMAL, w);
                samples[i] = (now() - start) * 1000;
            }
            output("fullscreen", n);
        } else if (!strcmp(command, "destroy") && fields == 1 && count) {
            unsigned n = count;
            for (unsigned i = 0; i < n; i++) {
                Window w = windows[--count];
                double start = now();
                XDestroyWindow(d, w);
                XFlush(d);
                wait_state(GONE, w);
                samples[i] = (now() - start) * 1000;
            }
            output("destroy", n);
        } else fail("unknown command, arity, or invalid phase order");
    }
    XCloseDisplay(d);
    return 0;
}
