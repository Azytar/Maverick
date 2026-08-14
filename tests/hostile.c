// Controllable, stdin-driven "hostile" X11 client for Maverick real-client
// compatibility (Fase 1/3/4/5/6 of the compatibility plan).
//
// Unlike stress.c (which loops argv-driven KIND/ITER), this client maps ONE
// window and then reads a command per line from stdin, so a shell harness
// (compat-matrix.sh) can drive precise EWMH/ICCCM sequences against a live,
// tiled Maverick session. It exercises the exact client behaviors that real
// apps exhibit:
//
//   - MapRequest / ConfigureRequest (incl. degenerate + huge geometry)
//   - synthetic ConfigureNotify (client "confirming" a geometry)
//   - _NET_ACTIVE_WINDOW ClientMessage (focus grab attempt)
//   - WM_TAKE_FOCUS (via WM_PROTOCOLS) — stress.c never sends this
//   - _NET_WM_STATE toggles (FULLSCREEN / MAXIMIZED_{VERT,HORZ})
//   - WM_TRANSIENT_FOR (build transient chains across processes)
//   - _NET_WM_DESKTOP (workspace requests)
//
// It prints `WINID=0x<hex>` once the window is mapped and `HOSTILE_DONE` on
// EOF, so the harness can capture the id and sequence commands. The objective
// is detection/observability only: it never benchmarks or backs off.
//
// Build (standalone, NOT part of the cargo workspace, like stress.c):
//   gcc tests/hostile.c -o /tmp/hostile -lX11
//
// Commands (one per line on stdin):
//   create                         map a window, print WINID
//   resize W H                     ConfigureRequest (client → WM)
//   move X Y                       ConfigureRequest move
//   configure X Y W H              ConfigureRequest full geometry
//   active                         _NET_ACTIVE_WINDOW ClientMessage
//   take_focus                     WM_TAKE_FOCUS (WM_PROTOCOLS) ClientMessage
//   fullscreen                     toggle _NET_WM_STATE_FULLSCREEN
//   maximize                       add MAXIMIZED_VERT|HORZ
//   unmaximize                     remove MAXIMIZED_VERT|HORZ
//   unmap                          UnmapWindow
//   remap                          MapWindow
//   destroy                        XDestroyWindow (and forget current)
//   transient <hexwin>             set WM_TRANSIENT_FOR for the NEXT create
//   workspace <n>                  _NET_WM_DESKTOP request (0-based)
//   spam-resize N                  N randomized ConfigureRequest/resize bursts
//   spam-active N                  N _NET_ACTIVE_WINDOW messages
//   spam-fullscreen N              N FULLSCREEN toggles
//
// Example transient chain across processes (Fase 6):
//   /tmp/hostile <<< $'create\n' &            # process A → prints WINIDA
//   /tmp/hostile <<< $'transient WINIDA\ncreate\n' &   # process B → child of A

#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static Display *dpy;
static Window root;
static Window cur = None;          // the currently-managed window (if any)
static Window pending_parent = None; // transient-for target for the next create

static Atom a_net_wm_state;
static Atom a_net_wm_window_type;
static Atom a_net_wm_window_type_dialog;
static Atom a_net_wm_window_type_normal;
static Atom a_net_wm_state_fullscreen;
static Atom a_net_wm_state_max_v;
static Atom a_net_wm_state_max_h;
static Atom a_net_active_window;
static Atom a_net_wm_desktop;
static Atom a_wm_protocols;
static Atom a_wm_take_focus;

// ── client-side counters (observability only) ────────────────────────────────
static long c_cfg_notify = 0, c_focus_in = 0, c_focus_out = 0, c_prop_state = 0;

static void intern_atoms(void) {
    a_net_wm_state          = XInternAtom(dpy, "_NET_WM_STATE", False);
    a_net_wm_window_type    = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE", False);
    a_net_wm_window_type_dialog = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_DIALOG", False);
    a_net_wm_window_type_normal = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_NORMAL", False);
    a_net_wm_state_fullscreen  = XInternAtom(dpy, "_NET_WM_STATE_FULLSCREEN", False);
    a_net_wm_state_max_v       = XInternAtom(dpy, "_NET_WM_STATE_MAXIMIZED_VERT", False);
    a_net_wm_state_max_h       = XInternAtom(dpy, "_NET_WM_STATE_MAXIMIZED_HORZ", False);
    a_net_active_window        = XInternAtom(dpy, "_NET_ACTIVE_WINDOW", False);
    a_net_wm_desktop          = XInternAtom(dpy, "_NET_WM_DESKTOP", False);
    a_wm_protocols            = XInternAtom(dpy, "WM_PROTOCOLS", False);
    a_wm_take_focus           = XInternAtom(dpy, "WM_TAKE_FOCUS", False);
}

// Synthetic ConfigureNotify: the client "confirming" a geometry it was told
// about. Selected StructureNotify below so our own loop can count it.
static void send_synthetic_configure(Window w, int x, int y, int ww, int hh) {
    XConfigureEvent ce;
    memset(&ce, 0, sizeof ce);
    ce.type = ConfigureNotify;
    ce.serial = 0;
    ce.send_event = True;
    ce.display = dpy;
    ce.event = w;
    ce.window = w;
    ce.x = x;
    ce.y = y;
    ce.width = ww;
    ce.height = hh;
    ce.border_width = 0;
    ce.above = None;
    ce.override_redirect = False;
    XSendEvent(dpy, w, False, StructureNotifyMask, (XEvent *)&ce);
}

// _NET_ACTIVE_WINDOW ClientMessage → root, requesting focus for `w`.
static void send_active_window(Window w) {
    XClientMessageEvent cm;
    memset(&cm, 0, sizeof cm);
    cm.type = ClientMessage;
    cm.serial = 0;
    cm.send_event = True;
    cm.display = dpy;
    cm.window = root;
    cm.message_type = a_net_active_window;
    cm.format = 32;
    cm.data.l[0] = 1; // source indication: normal application
    cm.data.l[1] = (long)w;
    cm.data.l[2] = CurrentTime;
    XSendEvent(dpy, root, False, SubstructureRedirectMask | SubstructureNotifyMask,
               (XEvent *)&cm);
}

// WM_TAKE_FOCUS (ICCCM §4.1.7) via WM_PROTOCOLS — a *focus request* the WM is
// expected to honor by sending a FocusIn; distinct from _NET_ACTIVE_WINDOW.
static void send_take_focus(Window w) {
    XClientMessageEvent cm;
    memset(&cm, 0, sizeof cm);
    cm.type = ClientMessage;
    cm.serial = 0;
    cm.send_event = True;
    cm.display = dpy;
    cm.window = w;
    cm.message_type = a_wm_protocols;
    cm.format = 32;
    cm.data.l[0] = (long)a_wm_take_focus;
    cm.data.l[1] = CurrentTime;
    XSendEvent(dpy, w, False, 0, (XEvent *)&cm);
}

// _NET_WM_STATE *toggle* (add when `add`>0, remove when 0) of one or two atoms.
static void toggle_state(Window w, Atom a1, Atom a2, int add) {
    XClientMessageEvent cm;
    memset(&cm, 0, sizeof cm);
    cm.type = ClientMessage;
    cm.serial = 0;
    cm.send_event = True;
    cm.display = dpy;
    cm.window = w;
    cm.message_type = a_net_wm_state;
    cm.format = 32;
    cm.data.l[0] = add ? 1 : 0;
    cm.data.l[1] = (long)a1;
    cm.data.l[2] = (long)a2;
    XSendEvent(dpy, root, False, SubstructureRedirectMask | SubstructureNotifyMask,
               (XEvent *)&cm);
}

static void set_state_prop(Window w, const Atom *atoms, int n) {
    XChangeProperty(dpy, w, a_net_wm_state, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)atoms, n);
}

static void set_wtype_prop(Window w, Atom t) {
    XChangeProperty(dpy, w, a_net_wm_window_type, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)&t, 1);
}

// Advertise WM_TAKE_FOCUS support so the WM will send us a FocusIn when it
// wants us focused (ICCCM). Without this the WM would use SetInputFocus only.
static void set_wm_protocols(Window w) {
    Atom protos[1] = { a_wm_take_focus };
    XChangeProperty(dpy, w, a_wm_protocols, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)protos, 1);
}

static Window make_window(void) {
    XSetWindowAttributes wa;
    memset(&wa, 0, sizeof wa);
    Window w = XCreateWindow(dpy, root, 100, 100, 300, 200, 0,
                             CopyFromParent, InputOutput, CopyFromParent, 0, &wa);

    XClassHint ch;
    char cn[32], cc[32];
    snprintf(cn, sizeof cn, "hostile");
    snprintf(cc, sizeof cc, "hostile");
    ch.res_name = cn;
    ch.res_class = cc;
    XSetClassHint(dpy, w, &ch);
    XStoreName(dpy, w, cn);

    XSelectInput(dpy, w,
                 StructureNotifyMask | FocusChangeMask | PropertyChangeMask);

    set_wtype_prop(w, a_net_wm_window_type_normal);
    set_wm_protocols(w);

    if (pending_parent != None) {
        XSetTransientForHint(dpy, w, pending_parent);
        pending_parent = None;
    }
    return w;
}

// Drain inbound events so we can count what the WM did to our window.
static void drain(void) {
    XEvent ev;
    while (XPending(dpy)) {
        XNextEvent(dpy, &ev);
        switch (ev.type) {
            case ConfigureNotify: c_cfg_notify++; break;
            case FocusIn:         c_focus_in++;  break;
            case FocusOut:        c_focus_out++; break;
            case PropertyNotify:
                if (ev.xproperty.atom == a_net_wm_state) c_prop_state++;
                break;
            default: break;
        }
    }
}

// Clamp to legal CARD16 sizes (X rejects 0 or >65535).
static int clamp_sz(int v) { return v > 0 ? (v > 65000 ? 65000 : v) : 1; }

static void do_create(void) {
    if (cur != None) return; // one window at a time
    cur = make_window();
    XMapWindow(dpy, cur);
    XFlush(dpy);
    printf("WINID=0x%lx\n", (unsigned long)cur);
    fflush(stdout);
}

static void do_destroy(void) {
    if (cur == None) return;
    XDestroyWindow(dpy, cur);
    XFlush(dpy);
    cur = None;
}

static void parse_hexwin(const char *s, Window *out) {
    if (!s) { *out = None; return; }
    // accept "0x.." or bare hex
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) s += 2;
    *out = (Window)strtoul(s, NULL, 16);
}

static void dispatch(char *line) {
    char *cmd = strtok(line, " \t\r\n");
    if (!cmd) return;

    if (!strcmp(cmd, "create")) {
        do_create();
    } else if (!strcmp(cmd, "destroy")) {
        do_destroy();
    } else if (!strcmp(cmd, "unmap")) {
        if (cur != None) { XUnmapWindow(dpy, cur); XFlush(dpy); }
    } else if (!strcmp(cmd, "remap")) {
        if (cur != None) { XMapWindow(dpy, cur); XFlush(dpy); }
    } else if (!strcmp(cmd, "active")) {
        if (cur != None) { send_active_window(cur); XFlush(dpy); }
    } else if (!strcmp(cmd, "take_focus")) {
        if (cur != None) { send_take_focus(cur); XFlush(dpy); }
    } else if (!strcmp(cmd, "fullscreen")) {
        if (cur != None) { toggle_state(cur, a_net_wm_state_fullscreen, None, 1); XFlush(dpy); }
    } else if (!strcmp(cmd, "maximize")) {
        if (cur != None) { toggle_state(cur, a_net_wm_state_max_v, a_net_wm_state_max_h, 1); XFlush(dpy); }
    } else if (!strcmp(cmd, "unmaximize")) {
        if (cur != None) { toggle_state(cur, a_net_wm_state_max_v, a_net_wm_state_max_h, 0); XFlush(dpy); }
    } else if (!strcmp(cmd, "move") || !strcmp(cmd, "resize") || !strcmp(cmd, "configure")) {
        int x = 100, y = 100, w = 300, h = 200;
        if (!strcmp(cmd, "move")) {
            char *a = strtok(NULL, " \t\r\n");
            char *b = strtok(NULL, " \t\r\n");
            if (a) x = atoi(a);
            if (b) y = atoi(b);
            if (cur != None) { XMoveWindow(dpy, cur, x, y); XFlush(dpy); }
        } else if (!strcmp(cmd, "resize")) {
            char *a = strtok(NULL, " \t\r\n");
            char *b = strtok(NULL, " \t\r\n");
            if (a) w = clamp_sz(atoi(a));
            if (b) h = clamp_sz(atoi(b));
            if (cur != None) { XResizeWindow(dpy, cur, w, h); XFlush(dpy); }
        } else { // configure X Y W H
            char *p[4]; int i = 0;
            while (i < 4 && (p[i] = strtok(NULL, " \t\r\n"))) i++;
            if (i == 4 && cur != None) {
                x = atoi(p[0]); y = atoi(p[1]);
                w = clamp_sz(atoi(p[2])); h = clamp_sz(atoi(p[3]));
                XMoveResizeWindow(dpy, cur, x, y, w, h);
                send_synthetic_configure(cur, x, y, w, h);
                XFlush(dpy);
            }
        }
    } else if (!strcmp(cmd, "transient")) {
        char *a = strtok(NULL, " \t\r\n");
        Window p; parse_hexwin(a, &p);
        if (p != None) pending_parent = p;
    } else if (!strcmp(cmd, "workspace")) {
        char *a = strtok(NULL, " \t\r\n");
        if (cur != None && a) {
            long n = atol(a);
            unsigned long v = (unsigned long)n;
            XChangeProperty(dpy, cur, a_net_wm_desktop, XA_CARDINAL, 32,
                            PropModeReplace, (const unsigned char *)&v, 1);
            XClientMessageEvent cm;
            memset(&cm, 0, sizeof cm);
            cm.type = ClientMessage;
            cm.send_event = True;
            cm.display = dpy;
            cm.window = cur;
            cm.message_type = a_net_wm_desktop;
            cm.format = 32;
            cm.data.l[0] = n;
            XSendEvent(dpy, root, False,
                       SubstructureRedirectMask | SubstructureNotifyMask,
                       (XEvent *)&cm);
            XFlush(dpy);
        }
    } else if (!strcmp(cmd, "spam-resize")) {
        char *a = strtok(NULL, " \t\r\n");
        long n = a ? atol(a) : 10;
        if (cur != None) {
            for (long i = 0; i < n; i++) {
                int w = clamp_sz(50 + (int)(i * 37) % 1200);
                int h = clamp_sz(40 + (int)(i * 53) % 800);
                XResizeWindow(dpy, cur, w, h);
                send_synthetic_configure(cur, 100 + (int)(i*13)%400, 100 + (int)(i*7)%300, w, h);
            }
            XFlush(dpy);
        }
    } else if (!strcmp(cmd, "spam-active")) {
        char *a = strtok(NULL, " \t\r\n");
        long n = a ? atol(a) : 10;
        if (cur != None) {
            for (long i = 0; i < n; i++) send_active_window(cur);
            XFlush(dpy);
        }
    } else if (!strcmp(cmd, "spam-fullscreen")) {
        char *a = strtok(NULL, " \t\r\n");
        long n = a ? atol(a) : 10;
        if (cur != None) {
            for (long i = 0; i < n; i++)
                toggle_state(cur, a_net_wm_state_fullscreen, None, i % 2);
            XFlush(dpy);
        }
    }
    // unknown commands are silently ignored (robust harness driving)
}

int main(int argc, char **argv) {
    if (argc >= 2) setenv("DISPLAY", argv[1], 1);
    dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "hostile: cannot open display\n"); return 1; }
    root = DefaultRootWindow(dpy);
    intern_atoms();

    char *line = NULL;
    size_t cap = 0;
    ssize_t len;
    while ((len = getline(&line, &cap, stdin)) != -1) {
        char buf[1024];
        snprintf(buf, sizeof buf, "%s", line);
        dispatch(buf);
        drain();
    }
    free(line);

    // Final tally (observability for the harness).
    printf("HOSTILE_STATS cfg_notify=%ld focus_in=%ld focus_out=%ld state_prop=%ld\n",
           c_cfg_notify, c_focus_in, c_focus_out, c_prop_state);
    printf("HOSTILE_DONE\n");
    fflush(stdout);

    if (cur != None) XDestroyWindow(dpy, cur);
    XCloseDisplay(dpy);
    return 0;
}
