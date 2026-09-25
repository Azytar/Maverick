// Test client for the Maverick partial-redraw harness.
//
// Creates one window painted a solid base colour. The legacy default is an
// indefinite full-refresh client: every tick repaints the entire window, which
// is what the existing harnesses use. The bounded small-damage mode paints a
// fixed sequence of small rectangles in one bounded batch and exits, so it can
// exercise damage accumulation without an unbounded full-window refresh.
//
// Usage:
//   damager [--full-refresh] [BASE_HEX [DOT_HEX [NAME]]]
//   damager --small-damage [--count N] [BASE_HEX [DOT_HEX [NAME]]]
//
//   BASE_HEX       solid window colour, e.g. 0x3399ff (default: 0x3399ff)
//   DOT_HEX        colour for the bounded rectangles (default: 0xff3366)
//   NAME           retained optional window-name argument
//   --small-damage  paint exactly N deterministic small rectangles, then exit
//   --count N       rectangle count (default: 32, maximum: 256)
//   --full-refresh  use the legacy indefinite full-window refresh (default)
//   -h, --help      show this usage
//
// The window is tagged WM_CLASS="damager" so xdotool can find/move it.

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define DEFAULT_SMALL_RECT_COUNT 32
#define MAX_SMALL_RECT_COUNT 256
#define SMALL_RECT_WIDTH 24
#define SMALL_RECT_HEIGHT 18

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

static void usage(const char *prog) {
    fprintf(stderr,
            "Usage: %s [--full-refresh] [BASE_HEX [DOT_HEX [NAME]]]\n"
            "       %s --small-damage [--count N] [BASE_HEX [DOT_HEX [NAME]]]\n"
            "  --small-damage  paint exactly N deterministic small rectangles, then exit\n"
            "  --count N       rectangle count (default: %d, maximum: %d)\n"
            "  --full-refresh  use the legacy indefinite full-window refresh (default)\n"
            "  -h, --help      show this usage\n",
            prog, prog, DEFAULT_SMALL_RECT_COUNT, MAX_SMALL_RECT_COUNT);
}

static int parse_count(const char *text, int *count) {
    char *end = NULL;
    long value;
    if (!text || *text == '\0') return 0;
    value = strtol(text, &end, 10);
    if (*end != '\0' || value < 1 || value > MAX_SMALL_RECT_COUNT) return 0;
    *count = (int)value;
    return 1;
}

static int is_decimal_count(const char *text) {
    int i;
    if (!text || *text == '\0') return 0;
    for (i = 0; text[i] != '\0'; i++) {
        if (text[i] < '0' || text[i] > '9') return 0;
    }
    return 1;
}

int main(int argc, char **argv) {
    int small_mode = 0;
    int small_count = DEFAULT_SMALL_RECT_COUNT;
    const char *positional[4] = {0};
    int positional_count = 0;
    int i;

    for (i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-h") == 0 || strcmp(argv[i], "--help") == 0) {
            usage(argv[0]);
            return 0;
        }
        if (strcmp(argv[i], "--small") == 0 ||
            strcmp(argv[i], "--small-damage") == 0 ||
            strcmp(argv[i], "--bounded-damage") == 0) {
            small_mode = 1;
            if (i + 1 < argc && is_decimal_count(argv[i + 1])) {
                if (!parse_count(argv[++i], &small_count)) {
                    usage(argv[0]);
                    return 2;
                }
            }
        } else if (strcmp(argv[i], "--full") == 0 ||
                   strcmp(argv[i], "--full-refresh") == 0) {
            small_mode = 0;
        } else if (strcmp(argv[i], "--count") == 0 ||
                   strcmp(argv[i], "--rectangles") == 0 ||
                   strcmp(argv[i], "--small-rects") == 0) {
            if (++i >= argc || !parse_count(argv[i], &small_count)) {
                usage(argv[0]);
                return 2;
            }
            small_mode = 1;
        } else if (strncmp(argv[i], "--count=", 8) == 0 ||
                   strncmp(argv[i], "--rectangles=", 13) == 0 ||
                   strncmp(argv[i], "--small-rects=", 14) == 0) {
            const char *value = strchr(argv[i], '=');
            if (!parse_count(value + 1, &small_count)) {
                usage(argv[0]);
                return 2;
            }
            small_mode = 1;
        } else if (argv[i][0] == '-') {
            usage(argv[0]);
            return 2;
        } else if (positional_count < 4) {
            positional[positional_count++] = argv[i];
        } else {
            usage(argv[0]);
            return 2;
        }
    }

    if (small_mode && positional_count > 3) {
        if (!parse_count(positional[3], &small_count)) {
            usage(argv[0]);
            return 2;
        }
    }

    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "damager: no display\n"); return 1; }
    int scr = DefaultScreen(d);
    unsigned long base = alloc_pixel(d, positional_count > 0 ? strtoul(positional[0], 0, 16) : 0x3399ff);
    unsigned long dot  = alloc_pixel(d, positional_count > 1 ? strtoul(positional[1], 0, 16) : 0xff3366);

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
    char name[128] = "damager";
    if (positional_count > 2) {
        snprintf(name, sizeof(name), "%s", positional[2]);
    }
    char cls[] = "damager";
    ch.res_name = name;
    ch.res_class = cls;
    XSetClassHint(d, w, &ch);
    XStoreName(d, w, name);

    GC gc = XCreateGC(d, w, 0, 0);
    XMapWindow(d, w);
    XRaiseWindow(d, w);
    XFlush(d);
    fprintf(stderr, "WINID=0x%lx\n", (unsigned long) w);
    fflush(stderr);

    if (small_mode) {
        int r;
        XSetForeground(d, gc, base);
        XFillRectangle(d, w, gc, 0, 0, 420, 320);
        XFlush(d);
        fprintf(stderr, "SMALL_RECTS=%d\n", small_count);
        fflush(stderr);
        for (r = 0; r < small_count; r++) {
            int x = 20 + (r * 37) % 360;
            int y = 20 + (r * 53) % 260;
            XSetForeground(d, gc, dot);
            XFillRectangle(d, w, gc, x, y, SMALL_RECT_WIDTH, SMALL_RECT_HEIGHT);
        }
        XFlush(d);
        XSync(d, False);
        usleep(100000);
        XCloseDisplay(d);
        return 0;
    }

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
    XCloseDisplay(d);
    return 0;
}
