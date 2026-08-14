// Move (and optionally resize) a window by id, for the partial-redraw harness.
// Uses XMoveWindow / XResizeWindow directly so it also works on override-redirect
// windows (which the tiling WM does not manage). The geometry change generates a
// ConfigureNotify the compositor turns into a full repaint.
//
// Usage: winmove WINID X Y [W H]

#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc < 4) { fprintf(stderr, "usage: winmove WINID X Y [W H]\n"); return 1; }
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "winmove: no display\n"); return 1; }
    Window w = (Window) strtoul(argv[1], 0, 0);
    int x = atoi(argv[2]), y = atoi(argv[3]);
    XMoveWindow(d, w, x, y);
    if (argc >= 6) XResizeWindow(d, w, atoi(argv[4]), atoi(argv[5]));
    XFlush(d);
    return 0;
}
