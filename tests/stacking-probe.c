#define _POSIX_C_SOURCE 200809L
// Cooperative X11 stacking probe. Used only on the isolated Xvfb test server.
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <poll.h>
#include <time.h>

static unsigned long *windows_property(Display *d, Window root, const char *name, unsigned long *count) {
    Atom type;
    int format;
    unsigned long remaining;
    unsigned char *data = NULL;
    if (XGetWindowProperty(d, root, XInternAtom(d, name, False), 0, 4096, False,
                           XA_WINDOW, &type, &format, count, &remaining, &data) != Success ||
        (type != None && (type != XA_WINDOW || format != 32 || remaining))) exit(2);
    return (unsigned long *)data;
}

static int contains(const unsigned long *values, unsigned long count, Window win) {
    for (unsigned long i = 0; i < count; i++) if (values[i] == win) return 1;
    return 0;
}

static void array(const unsigned long *values, unsigned long count) {
    putchar('[');
    for (unsigned long i = 0; i < count; i++) printf("%s%lu", i ? "," : "", values[i]);
    putchar(']');
}

static void stack_snapshot(Display *d, Window root, Window dock) {
    unsigned long nmanaged = 0, npublished = 0, nactive = 0;
    unsigned long *managed = windows_property(d, root, "_NET_CLIENT_LIST", &nmanaged);
    unsigned long *published = windows_property(d, root, "_NET_CLIENT_LIST_STACKING", &npublished);
    unsigned long *active = windows_property(d, root, "_NET_ACTIVE_WINDOW", &nactive);
    Window returned_root, parent, *children = NULL, focused;
    unsigned count = 0, used = 0;
    int revert;
    if (!XQueryTree(d, root, &returned_root, &parent, &children, &count) ||
        !XGetInputFocus(d, &focused, &revert)) exit(2);
    for (unsigned i = 0; i < count; i++)
        if (contains(managed, nmanaged, children[i]) || children[i] == dock) children[used++] = children[i];
    int same = used == npublished;
    for (unsigned i = 0; same && i < used; i++) same = children[i] == published[i];
    printf("{\"equal\":%s,\"focus\":%lu,\"active\":%lu,\"real\":", same ? "true" : "false", focused, nactive ? active[0] : 0);
    array(children, used);
    printf(",\"published\":"); array(published, npublished);
    printf(",\"managed\":"); array(managed, nmanaged);
    puts("}");
    if (children) XFree(children);
    if (managed) XFree(managed);
    if (published) XFree(published);
    if (active) XFree(active);
}

static unsigned long long monotonic_ns(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) exit(2);
    return (unsigned long long)now.tv_sec * 1000000000 + (unsigned long long)now.tv_nsec;
}

int main(int argc, char **argv) {
    int fullscreen = argc == 2 && !strcmp(argv[1], "--fullscreen");
    Window transient = None;
    if (argc == 3 && !strcmp(argv[1], "--transient-for")) {
        char *end;
        transient = strtoul(argv[2], &end, 0);
        if (!transient || *end) return 3;
    } else if (argc != 1 && !fullscreen) return 3;
    Display *d = XOpenDisplay(NULL);
    if (!d) return 1;
    Window root = DefaultRootWindow(d), dock = None, peer = None;
    Window win = XCreateSimpleWindow(d, root, 0, 0, 400, 300, 0, 0, 0xffffff);
    XClassHint initial_class = {"stage11", "stage11"};
    XSetClassHint(d, win, &initial_class);
    XStoreName(d, win, "stacking-probe");
    XSelectInput(d, win, StructureNotifyMask | PropertyChangeMask);
    XSelectInput(d, root, PropertyChangeMask);
    if (transient) XSetTransientForHint(d, win, transient);
    if (fullscreen) {
        Atom fullscreen = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
        XChangeProperty(d, win, XInternAtom(d, "_NET_WM_STATE", False), XA_ATOM, 32,
                        PropModeReplace, (unsigned char *)&fullscreen, 1);
    }
    XMapWindow(d, win);
    XSync(d, False);
    printf("%lu\n", win); fflush(stdout);
    char cmd[256];
    unsigned long long armed = 0;
    while (fgets(cmd, sizeof(cmd), stdin)) {
        if (!strncmp(cmd, "many ", 5)) {
            unsigned amount;
            int transients;
            if (sscanf(cmd + 5, "%u %d", &amount, &transients) != 2 || amount < 2 || amount > 200) return 3;
            Window list[200] = {win};
            for (unsigned i = 1; i < amount; i++) {
                list[i] = XCreateSimpleWindow(d, root, 10, 10, 300, 200, 0, 0, 0xffffff);
                XSetClassHint(d, list[i], &initial_class);
                if (transients && i % 2) XSetTransientForHint(d, list[i], list[i - 1]);
                XMapWindow(d, list[i]);
            }
            XSync(d, False);
            array(list, amount); putchar('\n'); fflush(stdout);
            continue;
        } else if (!strcmp(cmd, "stack\n")) {
            stack_snapshot(d, root, dock); fflush(stdout); continue;
        } else if (!strcmp(cmd, "arm\n")) {
            XSync(d, False);
            while (XPending(d)) { XEvent event; XNextEvent(d, &event); }
            armed = monotonic_ns();
            puts("{\"armed\":true}"); fflush(stdout); continue;
        } else if (!strncmp(cmd, "complete ", 9)) {
            Window target;
            if (sscanf(cmd + 9, "%lu", &target) != 1 || !armed) return 3;
            Atom active_atom = XInternAtom(d, "_NET_ACTIVE_WINDOW", False);
            unsigned long long start = armed;
            int acknowledged = 0, done = 0;
            while (monotonic_ns() - start < 5000000000ULL) {
                while (XPending(d)) {
                    XEvent event; XNextEvent(d, &event);
                    if (event.type == PropertyNotify && event.xproperty.window == root &&
                        event.xproperty.atom == active_atom) acknowledged = 1;
                }
                unsigned long n = 0;
                unsigned long *active = windows_property(d, root, "_NET_ACTIVE_WINDOW", &n);
                Window focused; int revert;
                XGetInputFocus(d, &focused, &revert);
                done = acknowledged && n == 1 && active[0] == target && focused == target;
                if (active) XFree(active);
                if (done) break;
                struct pollfd fd = {ConnectionNumber(d), POLLIN, 0};
                poll(&fd, 1, 5);
            }
            printf("{\"complete\":%s,\"focus_ack_ns\":%llu}\n", done ? "true" : "false", monotonic_ns() - start);
            armed = 0; fflush(stdout); continue;
        } else if (!strncmp(cmd, "external ", 9)) {
            Window target; int bypass, above;
            if (sscanf(cmd + 9, "%lu %d %d", &target, &bypass, &above) != 3) return 3;
            XSetWindowAttributes attr = {0};
            if (bypass) { attr.override_redirect = True; XChangeWindowAttributes(d, target, CWOverrideRedirect, &attr); }
            if (above) XRaiseWindow(d, target); else XLowerWindow(d, target);
            if (bypass) { attr.override_redirect = False; XChangeWindowAttributes(d, target, CWOverrideRedirect, &attr); }
            XSync(d, False); puts("{\"external\":true}"); fflush(stdout); continue;
        } else if (!strncmp(cmd, "destroy-id ", 11)) {
            Window target;
            if (sscanf(cmd + 11, "%lu", &target) != 1) return 3;
            XDestroyWindow(d, target); XSync(d, False);
            puts("{\"destroyed\":true}"); fflush(stdout); continue;
        } else if (!strcmp(cmd, "focus-state\n")) {
            Window focused;
            int revert, format;
            Atom type;
            unsigned long count, remaining, active = None;
            unsigned char *data = NULL;
            XGetInputFocus(d, &focused, &revert);
            if (XGetWindowProperty(d, root, XInternAtom(d, "_NET_ACTIVE_WINDOW", False),
                                   0, 1, False, XA_WINDOW, &type, &format, &count,
                                   &remaining, &data) != Success || type != XA_WINDOW ||
                format != 32 || count != 1 || remaining || !data) return 2;
            active = *(unsigned long *)data;
            XFree(data);
            XWindowAttributes attr;
            if (!XGetWindowAttributes(d, win, &attr)) return 2;
            unsigned long pixel = 0;
            int y = attr.y + attr.border_width + attr.height / 2;
            if (focused == win && active == win && attr.map_state == IsViewable &&
                attr.border_width > 0 && attr.x >= 0 && attr.x < DisplayWidth(d, DefaultScreen(d)) &&
                y >= 0 && y < DisplayHeight(d, DefaultScreen(d))) {
                XImage *image = XGetImage(d, root, attr.x, y, 1, 1, AllPlanes, ZPixmap);
                if (!image) return 2;
                pixel = XGetPixel(image, 0, 0);
                XDestroyImage(image);
            }
            printf("%lu %lu %lu\n", focused, active, pixel);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "dockid", 6)) {
            printf("%lu\n", dock); fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "dock", 4)) {
            dock = XCreateSimpleWindow(d, root, 0, 0, 800, 24, 0, 0, 0xff0000);
            Atom type = XInternAtom(d, "_NET_WM_WINDOW_TYPE_DOCK", False);
            unsigned long strut[12] = {0, 0, 24, 0, 0, 0, 0, 0, 0, 799, 0, 0};
            XChangeProperty(d, dock, XInternAtom(d, "_NET_WM_WINDOW_TYPE", False),
                            XA_ATOM, 32, PropModeReplace, (unsigned char *)&type, 1);
            XChangeProperty(d, dock, XInternAtom(d, "_NET_WM_STRUT_PARTIAL", False),
                            XA_CARDINAL, 32, PropModeReplace, (unsigned char *)strut, 12);
            XMapWindow(d, dock);
        } else if (!strncmp(cmd, "remove", 6)) {
            if (dock) XDestroyWindow(d, dock);
            dock = None;
        } else if (!strncmp(cmd, "unpeer", 6)) {
            if (peer) XDestroyWindow(d, peer);
            peer = None;
        } else if (!strncmp(cmd, "events", 6)) {
            unsigned configures = 0, properties = 0;
            XSync(d, False);
            while (XPending(d)) {
                XEvent event;
                XNextEvent(d, &event);
                if (event.type == ConfigureNotify) configures++;
                if (event.type == PropertyNotify) properties++;
            }
            printf("%u %u\n", configures, properties); fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "state ", 6)) {
            long action;
            char first[64], second[64];
            int fields = sscanf(cmd + 6, "%ld %63s %63s", &action, first, second);
            if (fields < 2) return 3;
            XEvent event = {0};
            event.xclient.type = ClientMessage;
            event.xclient.window = win;
            event.xclient.message_type = XInternAtom(d, "_NET_WM_STATE", False);
            event.xclient.format = 32;
            event.xclient.data.l[0] = action;
            event.xclient.data.l[1] = XInternAtom(d, first, False);
            event.xclient.data.l[2] = fields == 3 ? XInternAtom(d, second, False) : None;
            event.xclient.data.l[3] = 1;
            XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &event);
        } else if (!strncmp(cmd, "fs", 2)) {
            XEvent e = {0};
            e.xclient.type = ClientMessage;
            e.xclient.window = win;
            e.xclient.message_type = XInternAtom(d, "_NET_WM_STATE", False);
            e.xclient.format = 32;
            e.xclient.data.l[0] = 1;
            e.xclient.data.l[1] = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
            e.xclient.data.l[3] = 1;
            XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
        } else if (!strncmp(cmd, "peer", 4)) {
            /* Map a second, ordinary managed tile (no dock struts, no name
             * match needed: the harness tracks the returned id). */
            peer = XCreateSimpleWindow(d, root, 0, 0, 300, 200, 0, 0, 0x00ff00);
            XStoreName(d, peer, "stacking-peer");
            XMapWindow(d, peer);
        } else if (!strncmp(cmd, "check", 5)) {
            Window r, parent, *children = NULL;
            unsigned int count = 0;
            int wi = -1, di = -1;
            Window checked_dock = dock;
            sscanf(cmd, "check %lu", &checked_dock);
            XWindowAttributes a;
            if (!XQueryTree(d, root, &r, &parent, &children, &count)) return 2;
            for (unsigned int i = 0; i < count; i++) {
                if (children[i] == win) wi = (int)i;
                if (children[i] == checked_dock) di = (int)i;
            }
            if (children) XFree(children);
            XGetWindowAttributes(d, win, &a);
            printf("%d %d %d %d %d %d %d\n", wi, di, a.x, a.y,
                   a.width, a.height, a.border_width);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "pixel", 5)) {
            XImage *image = XGetImage(d, root, 400, 12, 1, 1, AllPlanes, ZPixmap);
            if (!image) return 2;
            printf("%lu\n", XGetPixel(image, 0, 0));
            XDestroyImage(image);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "pcheck", 6)) {
            /* Peer-tile regression report: "wi pi px py pw ph pbw".
             * XQueryTree lists bottom-to-top, so A above B == wi > pi. */
            Window r, parent, *children = NULL;
            unsigned int count = 0;
            int wi = -1, pi = -1;
            XWindowAttributes pa;
            if (!XQueryTree(d, root, &r, &parent, &children, &count)) return 2;
            for (unsigned int i = 0; i < count; i++) {
                if (children[i] == win) wi = (int)i;
                if (children[i] == peer) pi = (int)i;
            }
            if (children) XFree(children);
            int px = -1, py = -1;
            unsigned pw = 0, ph = 0, pbw = 0;
            if (peer && XGetWindowAttributes(d, peer, &pa)) {
                px = pa.x; py = pa.y; pw = (unsigned)pa.width;
                ph = (unsigned)pa.height; pbw = (unsigned)pa.border_width;
            }
            printf("%d %d %d %d %u %u %u\n", wi, pi, px, py, pw, ph, pbw);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "quit", 4)) {
            break;
        } else return 3;
        XSync(d, False);
        puts("ok"); fflush(stdout);
    }
    XCloseDisplay(d);
    return 0;
}
