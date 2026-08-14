// Test client for the Maverick partial-redraw harness.
//
// Creates one window painted a solid base colour. In "small" mode it repaints
// only a small moving inner rectangle every tick, generating a stream of small
// XDamage events (the compositor sees a content-damage window, not a move, so
// it takes the partial-redraw path). The client erases the previous dot before
// drawing the next one, so the compositor must redraw the erased area too — if
// the partial path's damage accumulation is wrong, the old dot colour lingers
// as residue.
//
// Usage: damager BASE_HEX [DOT_HEX] [NAME]
//   BASE_HEX  solid window colour, e.g. 0x3399ff
//   DOT_HEX   moving inner-dot colour (default: a contrasting value)
//
// The window is tagged WM_CLASS="damager" so xdotool can find/move it.

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

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
    if (!d) { fprintf(stderr, "damager: no display\n"); return 1; }
    int scr = DefaultScreen(d);
    unsigned long base = alloc_pixel(d, argc > 1 ? strtoul(argv[1], 0, 16) : 0x3399ff);
    unsigned long dot  = alloc_pixel(d, argc > 2 ? strtoul(argv[2], 0, 16) : 0xff3366);

    // Override-redirect so the tiling WM leaves the window exactly where we put
    // it — the harness samples absolute screen coordinates, and a tiled window
    // would be relocated. The compositor still redirects + textures it.
    XSetWindowAttributes wa;
    wa.override_redirect = True;
    Window w = XCreateWindow(d, RootWindow(d, scr), 200, 200, 420, 320, 0,
                             CopyFromParent, InputOutput, CopyFromParent,
                             CWOverrideRedirect, &wa);
    XSelectInput(d, w, ExposureMask | StructureNotifyMask);

    // Tag WM_CLASS so the harness can address the window.
    XClassHint ch;
    char name[] = "damager";
    char cls[] = "damager";
    ch.res_name = name;
    ch.res_class = cls;
    XSetClassHint(d, w, &ch);

    GC gc = XCreateGC(d, w, 0, 0);
    XMapWindow(d, w);
    fprintf(stderr, "WINID=0x%lx\n", (unsigned long) w);
    fflush(stderr);

    int px = 30, py = 30, ppx = -100, ppy = -100; // previous dot, off-window first
    int frame = 0;
    int running = 1;
    while (running) {
        // Repaint the whole window base on first expose / resize.
        XSetForeground(d, gc, base);
        XFillRectangle(d, w, gc, 0, 0, 420, 320);
        // Erase the previous dot (small damage) then draw the new one (small
        // damage at a new location) — two small repaints per tick. After frame
        // 30 the dot settles at a fixed spot, so every earlier dot position has
        // been erased by the client and the compositor must redraw it back to the
        // base colour (the residue test samples one such spot).
        if (ppx >= 0) {
            XSetForeground(d, gc, base);
            XFillRectangle(d, w, gc, ppx, ppy, 40, 40);
        }
        if (frame <= 30) {
            px = 30 + (frame * 7) % 350;
            py = 30 + (frame * 11) % 250;
        } else {
            px = 300; py = 320;
        }
        XSetForeground(d, gc, dot);
        XFillRectangle(d, w, gc, px, py, 40, 40);
        XFlush(d);
        ppx = px; ppy = py;

        while (XPending(d)) {
            XEvent e;
            XNextEvent(d, &e);
            if (e.type == Expose) {
                XSetForeground(d, gc, base);
                XFillRectangle(d, w, gc, 0, 0, 420, 320);
            } else if (e.type == ConfigureNotify) {
                XSetForeground(d, gc, base);
                XFillRectangle(d, w, gc, 0, 0, 420, 320);
            } else if (e.type == ClientMessage) {
                running = 0;
            }
        }
        frame++;
        usleep(40000);
    }
    return 0;
}
