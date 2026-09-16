// setmon — split one X output into two RANDR 1.5 monitors without xrandr(1).
//
// Some xrandr builds fail to issue RRSetMonitor (they print the output names
// and change nothing); the server itself honours the request fine. This helper
// sends XRRSetMonitor directly for MON-L (left half, all outputs) and MON-R
// (right half, no outputs — same as `xrandr ... none`).
//
// There is a second trap on some Xorg builds (observed 21.1.24/Xvfb/Xephyr):
// client-created monitors are discarded when the creating connection closes,
// so a fire-and-forget setter (including xrandr itself) has no lasting effect.
// `--hold` keeps the connection open (pause until SIGTERM) so the monitors
// survive for the whole test; the harness must kill the keeper at cleanup.
//
// Usage: setmon [--hold] <display> <total_w> <height> <half_w>
// Prints the resulting monitor count ("monitors: N") and exits 0 iff N == 2
// (with --hold the count is printed before holding).
//
// Build: cc -O2 -o setmon setmon.c -lX11 -lXrandr
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/extensions/Xrandr.h>

int main(int argc, char **argv) {
    int hold = 0;
    int a = 1;
    if (a < argc && strcmp(argv[a], "--hold") == 0) {
        hold = 1;
        a++;
    }
    if (argc - a != 4) {
        fprintf(stderr, "usage: %s [--hold] <display> <total_w> <height> <half_w>\n", argv[0]);
        return 2;
    }
    int total_w = atoi(argv[a + 1]);
    int h = atoi(argv[a + 2]);
    int half = atoi(argv[a + 3]);
    if (total_w <= 0 || h <= 0 || half <= 0 || half >= total_w) {
        fprintf(stderr, "setmon: bad geometry\n");
        return 2;
    }

    Display *dpy = XOpenDisplay(argv[a]);
    if (!dpy) {
        fprintf(stderr, "setmon: cannot open %s\n", argv[1]);
        return 1;
    }
    Window root = DefaultRootWindow(dpy);

    XRRScreenResources *res = XRRGetScreenResources(dpy, root);
    RROutput primary_out = None;
    if (res && res->noutput > 0)
        primary_out = res->outputs[0];

    XRRMonitorInfo l = {0};
    l.name = XInternAtom(dpy, "MON-L", False);
    l.primary = False;
    l.automatic = False;
    l.x = 0;
    l.y = 0;
    l.width = half;
    l.height = h;
    l.mwidth = 340;
    l.mheight = 212;
    l.noutput = (primary_out != None) ? 1 : 0;
    l.outputs = (primary_out != None) ? &primary_out : NULL;
    XRRSetMonitor(dpy, root, &l);

    XRRMonitorInfo r = {0};
    r.name = XInternAtom(dpy, "MON-R", False);
    r.primary = False;
    r.automatic = False;
    r.x = half;
    r.y = 0;
    r.width = total_w - half;
    r.height = h;
    r.mwidth = 340;
    r.mheight = 212;
    r.noutput = 0;
    r.outputs = NULL;
    XRRSetMonitor(dpy, root, &r);
    XSync(dpy, False);

    int n = 0;
    XRRMonitorInfo *mons = XRRGetMonitors(dpy, root, True, &n);
    if (mons)
        XRRFreeMonitors(mons);
    if (res)
        XRRFreeScreenResources(res);

    printf("monitors: %d\n", n);
    fflush(stdout);
    if (n != 2) {
        XCloseDisplay(dpy);
        return 1;
    }
    if (hold) {
        // Keep this connection open until SIGTERM: some Xorg builds discard
        // client-created monitors when the creator disconnects.
        for (;;)
            pause();
    }
    XCloseDisplay(dpy);
    return 0;
}
