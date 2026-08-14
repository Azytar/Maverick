// Managed (tiled) window client for Maverick end-to-end harnesses.
//
// Maps a *normal* (managed, non-override-redirect) window so Maverick tracks it
// in its client set — required for fullscreen / focus / layout / kill tests.
//
// Modes (argv):
//   (default)  cooperative  : advertises WM_DELETE_WINDOW and honours it.
//   nowm       non-coop     : does NOT advertise WM_DELETE_WINDOW (the WM must
//                             force-kill it); also ignores ClientMessage.
//   lie        non-coop*    : advertises WM_DELETE_WINDOW but IGNORES the delete
//                             request — forces the WM to wait out its shutdown
//                             budget then force-kill. The worst case for a
//                             graceful-shutdown test.
//
// Usage: mgdwin [nowm|lie]

#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "mgdwin: no display\n"); return 1; }
    int scr = DefaultScreen(d);

    int w = 520, h = 360;
    Window win = XCreateSimpleWindow(d, RootWindow(d, scr), 60, 60, w, h, 2,
                                     BlackPixel(d, scr), WhitePixel(d, scr));

    const char *title = getenv("MGDTITLE");
    if (!title || !*title) title = "mgdwin";
    XStoreName(d, win, title);
    XClassHint ch; char n[] = "mgdwin", c[] = "mgdwin";
    ch.res_name = n; ch.res_class = c;
    XSetClassHint(d, win, &ch);

    Atom net_wm_name = XInternAtom(d, "_NET_WM_NAME", False);
    Atom utf8        = XInternAtom(d, "UTF8_STRING", False);
    XChangeProperty(d, win, net_wm_name, utf8, 8, PropModeReplace,
                    (unsigned char *)title, strlen(title));

    int mode = 0; // 0 cooperative, 1 nowm, 2 lie
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "nowm")) mode = 1;
        else if (!strcmp(argv[i], "lie")) mode = 2;
    }

    Atom wm_delete    = XInternAtom(d, "WM_DELETE_WINDOW", False);
    Atom wm_protocols = XInternAtom(d, "WM_PROTOCOLS", False);
    if (mode != 1) {
        XChangeProperty(d, win, wm_protocols, XA_ATOM, 32, PropModeReplace,
                        (unsigned char *)&wm_delete, 1);
    }

    XSelectInput(d, win, ExposureMask | StructureNotifyMask);
    XMapWindow(d, win);
    fprintf(stderr, "WINID=0x%lx\n", (unsigned long)win);
    fflush(stderr);

    XColor col; col.red = 0x22 * 257; col.green = 0x66 * 257; col.blue = 0xcc * 257;
    col.flags = DoRed | DoGreen | DoBlue;
    XAllocColor(d, DefaultColormap(d, scr), &col);
    GC gc = XCreateGC(d, win, 0, 0);
    XSetForeground(d, gc, col.pixel);

    int running = 1;
    while (running) {
        XEvent e;
        XNextEvent(d, &e);
        if (e.type == Expose || e.type == ConfigureNotify) {
            XFillRectangle(d, win, gc, 0, 0, w, h);
            XFlush(d);
        } else if (e.type == ClientMessage) {
            Atom a = (Atom)e.xclient.data.l[0];
            if (a == wm_delete && mode != 1) {
                if (mode == 0) running = 0; // cooperative: exit cleanly
                // mode == 2 (lie): ignore the delete request
            }
        }
    }
    return 0;
}
