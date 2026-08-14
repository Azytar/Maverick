// Static solid-colour window for the Maverick partial-redraw harness.
//
// Paints a solid colour and stays alive, repainting only on Expose /
// ConfigureNotify (so moving/resizing it via the WM produces a fresh paint but
// it does not spam damage). Used as a backdrop and as the window for
// structural (map/unmap/resize) scenarios.
//
// Usage: staticwin X Y W H COLOR_HEX

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>

static unsigned long alloc_pixel(Display *d, unsigned long hex) {
    Colormap cm = DefaultColormap(d, DefaultScreen(d));
    XColor c;
    c.red = ((hex >> 16) & 0xff) * 257;
    c.green = ((hex >> 8) & 0xff) * 257;
    c.blue = (hex & 0xff) * 257;
    c.flags = DoRed | DoGreen | DoBlue;
    XAllocColor(d, cm, &c);
    return c.pixel;
}

int main(int argc, char **argv) {
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "staticwin: no display\n"); return 1; }
    int scr = DefaultScreen(d);
    int x = argc > 1 ? atoi(argv[1]) : 100;
    int y = argc > 2 ? atoi(argv[2]) : 100;
    int w = argc > 3 ? atoi(argv[3]) : 400;
    int h = argc > 4 ? atoi(argv[4]) : 300;
    unsigned long col = alloc_pixel(d, argc > 5 ? strtoul(argv[5], 0, 16) : 0x223355);

    // Override-redirect: the tiling WM must not relocate it (the harness samples
    // absolute coordinates). The compositor still composites it.
    XSetWindowAttributes wa;
    wa.override_redirect = True;
    Window win = XCreateWindow(d, RootWindow(d, scr), x, y, w, h, 0,
                               CopyFromParent, InputOutput, CopyFromParent,
                               CWOverrideRedirect, &wa);
    XSelectInput(d, win, ExposureMask | StructureNotifyMask);
    XClassHint ch; char n[] = "staticwin", c[] = "staticwin";
    ch.res_name = n; ch.res_class = c; XSetClassHint(d, win, &ch);
    GC gc = XCreateGC(d, win, 0, 0);
    XMapWindow(d, win);
    fprintf(stderr, "WINID=0x%lx\n", (unsigned long) win);
    fflush(stderr);

    int running = 1;
    while (running) {
        XEvent e;
        XNextEvent(d, &e);
        if (e.type == Expose || e.type == ConfigureNotify) {
            XSetForeground(d, gc, col);
            XFillRectangle(d, win, gc, 0, 0, w, h);
            XFlush(d);
        } else if (e.type == ClientMessage) {
            running = 0;
        }
    }
    return 0;
}
