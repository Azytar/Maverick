// Minimal dock client for the EWMH workarea harness.
//
// Maps a `_NET_WM_WINDOW_TYPE_DOCK` window with a top `_NET_WM_STRUT_PARTIAL`
// reservation so Maverick shrinks the workarea and republishes `_NET_WORKAREA`.
//
// Usage: dockstrut [TOP_PX]   (default 30). Prints DOCK=0x... and sleeps so the
// harness can probe root properties, then exits on SIGTERM (the harness kills
// it to verify the workarea is restored).

#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    int top = argc > 1 ? atoi(argv[1]) : 30;
    Display *d = XOpenDisplay(NULL);
    if (!d) {
        fprintf(stderr, "dockstrut: no display\n");
        return 1;
    }
    int scr = DefaultScreen(d);
    int w = DisplayWidth(d, scr);
    Window root = RootWindow(d, scr);
    Window win =
        XCreateSimpleWindow(d, root, 0, 0, (unsigned)w, (unsigned)top, 0, 0, 0);

    Atom wt = XInternAtom(d, "_NET_WM_WINDOW_TYPE", False);
    Atom dock = XInternAtom(d, "_NET_WM_WINDOW_TYPE_DOCK", False);
    XChangeProperty(d, win, wt, XA_ATOM, 32, PropModeReplace,
                    (unsigned char *)&dock, 1);
    Atom sp = XInternAtom(d, "_NET_WM_STRUT_PARTIAL", False);
    long s[12] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    s[2] = top; // top thickness
    s[8] = 0; // top_start_x
    s[9] = w - 1; // top_end_x
    XChangeProperty(d, win, sp, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)s, 12);
    Atom st = XInternAtom(d, "_NET_WM_STRUT", False);
    long s4[4] = {0, 0, 0, 0};
    s4[2] = top;
    XChangeProperty(d, win, st, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)s4, 4);
    XStoreName(d, win, "maverick-dockstrut");
    XMapWindow(d, win);
    XFlush(d);
    printf("DOCK=0x%lx W=%d top=%d\n", (unsigned long)win, w, top);
    fflush(stdout);
    for (;;) {
        pause();
    }
    return 0;
}
