// Real managed X11 client for Maverick fullscreen reproduction.
//
// Behavior:
// - Normal managed window (no override-redirect).
// - Repaints on Expose/ConfigureNotify with a static coloured rectangle and a
//   moving dot so the WM sees Damage.
// - SIGUSR1 toggles _NET_WM_STATE_FULLSCREEN via a ClientMessage.
// - Prints WINID=0x... on stderr for harness parsing.
// - SIGUSR2 writes geometry/state snapshots to /tmp/realwin-state-<pid>.
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <fcntl.h>
#include <unistd.h>

static volatile sig_atomic_t g_fullscreen;
static volatile sig_atomic_t g_snapshot;

static void on_usr1(int sig) { (void)sig; g_fullscreen = 1; }
static void on_usr2(int sig) { (void)sig; g_snapshot = 1; }

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

static void write_state(Display *d, Window win, int fs) {
    XWindowAttributes a;
    if (!XGetWindowAttributes(d, win, &a)) return;
    char path[256];
    snprintf(path, sizeof(path), "/tmp/realwin-state-%d.tmp", getpid());
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) return;
    dprintf(fd,
            "win=0x%lx x=%d y=%d w=%u h=%u bw=%u viewable=%d fs=%d\n",
            (unsigned long)win, a.x, a.y, a.width, a.height, a.border_width,
            a.map_state == IsViewable, fs);
    close(fd);
    char final[256];
    snprintf(final, sizeof(final), "/tmp/realwin-state-%d", getpid());
    rename(path, final);
}

int main(int argc, char **argv) {
    const char *dpy_name = getenv("DISPLAY");
    Display *d = XOpenDisplay(dpy_name);
    if (!d) { fprintf(stderr, "realwin: no display %s\n", dpy_name ? dpy_name : ""); return 1; }
    int scr = DefaultScreen(d);
    int x = argc > 1 ? atoi(argv[1]) : 80;
    int y = argc > 2 ? atoi(argv[2]) : 60;
    int w = argc > 3 ? atoi(argv[3]) : 640;
    int h = argc > 4 ? atoi(argv[4]) : 400;
    unsigned long base = alloc_pixel(d, argc > 5 ? strtoul(argv[5], 0, 16) : 0x2266cc);
    unsigned long dot = 0xffffff - base;

    Window win = XCreateSimpleWindow(d, RootWindow(d, scr), x, y, w, h, 2,
                                    BlackPixel(d, scr), base);
    XSelectInput(d, win, ExposureMask | StructureNotifyMask | PropertyChangeMask);
    XClassHint ch; char n[] = "realwin", c[] = "realwin";
    ch.res_name = n; ch.res_class = c; XSetClassHint(d, win, &ch);
    Atom net_wm_name = XInternAtom(d, "_NET_WM_NAME", False);
    Atom utf8 = XInternAtom(d, "UTF8_STRING", False);
    Atom net_wm_state = XInternAtom(d, "_NET_WM_STATE", False);
    Atom net_wm_state_fs = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
    Atom wm_protocols = XInternAtom(d, "WM_PROTOCOLS", False);
    Atom wm_delete = XInternAtom(d, "WM_DELETE_WINDOW", False);
    XChangeProperty(d, win, wm_protocols, XA_ATOM, 32, PropModeReplace,
                    (unsigned char *)&wm_delete, 1);
    XMapWindow(d, win);
    XFlush(d);
    fprintf(stderr, "WINID=0x%lx\n", (unsigned long)win);
    fflush(stderr);

    struct sigaction sa = {0};
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, NULL);
    sa.sa_handler = on_usr2;
    sigaction(SIGUSR2, &sa, NULL);

    GC gc = DefaultGC(d, scr);
    int dot_x = 0, dot_y = 0, ppx = -100, ppy = -100, frame = 0, fs_state = 0;
    while (1) {
        while (XPending(d)) {
            XEvent ev;
            XNextEvent(d, &ev);
            if (ev.type == ClientMessage) {
                XClientMessageEvent *cm = (XClientMessageEvent *)&ev;
                if (cm->message_type == net_wm_state) {
                    long state = cm->data.l[1];
                    long action = cm->data.l[0];
                    if (state == net_wm_state_fs && (action == 1 || action == 0)) {
                        fs_state = (action == 1) ? 1 : 0;
                    }
                }
            }
        }
        if (g_fullscreen) {
            XClientMessageEvent cm = {0};
            cm.type = ClientMessage;
            cm.window = win;
            cm.message_type = net_wm_state;
            cm.format = 32;
            cm.data.l[0] = fs_state ? 0 : 1;
            cm.data.l[1] = net_wm_state_fs;
            cm.data.l[2] = 0;
            cm.data.l[3] = 1;
            cm.data.l[4] = 0;
            XSendEvent(d, DefaultRootWindow(d), False,
                       SubstructureNotifyMask | SubstructureRedirectMask,
                       (XEvent *)&cm);
            XFlush(d);
            g_fullscreen = 0;
        }
        XSetForeground(d, gc, base);
        XFillRectangle(d, win, gc, 0, 0, w, h);
        if (ppx >= 0) {
            XSetForeground(d, gc, base);
            XFillRectangle(d, win, gc, ppx, ppy, 60, 60);
        }
        dot_x = (frame * 11) % (w - 60);
        dot_y = (frame * 17) % (h - 60);
        XSetForeground(d, gc, dot);
        XFillRectangle(d, win, gc, dot_x, dot_y, 60, 60);
        XFlush(d);
        if (g_snapshot) {
            write_state(d, win, fs_state);
            g_snapshot = 0;
        }
        ppx = dot_x; ppy = dot_y;
        frame++;
        usleep(16000);
    }
    return 0;
}
