// Hostile X11 stress client for Maverick risk-closure (Riesgo 7).
//
// Connects to the display, creates a window, and — per iteration — hammers the
// window manager with the 8 hostile flows called out in the task:
//
//   1. Map
//   2. ConfigureRequest (varying geometry, including degenerate 0x0 and huge)
//   3. ConfigureNotify  (synthetic, mimicking the client "confirming" a geom)
//   4. _NET_ACTIVE_WINDOW  ClientMessage
//   5. Resize
//   6. Unmap
//   7. Remap
//   8. Destroy
//
// It runs these across window *kinds* so maverick treats the window as:
//   tiled | fullscreen | float | maximize | transient/modal
// by setting the appropriate EWMH/ICCCM hints BEFORE mapping.
//
// The objective is DETECTION only (no benchmark, no WM backoff):
//   - infinite loops / ConfigureNotify storms
//   - focus oscillation (FocusIn/FocusOut churn, _NET_ACTIVE_WINDOW storms)
//   - stale AppliedState / stale Desired (geometry divergence)
//   - zombie presented_maximize / zombie pending_focus (leftover state)
//
// It counts everything it can observe from the client side and prints a
// per-kind summary. Abnormal: if maverick enters an infinite loop the X calls
// block and the client never prints the STRESS_DONE marker (the harness flags
// this as a HANG).
//
// Usage: stress KIND ITERATIONS [DISPLAY]
//   KIND ∈ tiled fullscreen float maximize transient

#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum { KIND_TILE = 0, KIND_FS, KIND_FLOAT, KIND_MAX, KIND_TRANS, KIND_COUNT };

static const char *kind_name(int k) {
    switch (k) {
        case KIND_TILE:  return "tiled";
        case KIND_FS:    return "fullscreen";
        case KIND_FLOAT: return "float";
        case KIND_MAX:   return "maximize";
        case KIND_TRANS: return "transient";
        default:         return "?";
    }
}

static Display *dpy;
static Window root;

static Atom a_net_wm_state;
static Atom a_net_wm_window_type;
static Atom a_net_wm_window_type_dialog;
static Atom a_net_wm_window_type_normal;
static Atom a_net_wm_state_fullscreen;
static Atom a_net_wm_state_max_v;
static Atom a_net_wm_state_max_h;
static Atom a_net_wm_state_modal;
static Atom a_net_active_window;

// ── client-side counters ────────────────────────────────────────────────────
static long c_map = 0, c_unmap = 0, c_remap = 0, c_destroy = 0;
static long c_cfg_req = 0, c_resize = 0, c_active = 0;
static long c_cfg_notify = 0, c_focus_in = 0, c_focus_out = 0, c_prop_state = 0;

static void intern_atoms(void) {
    a_net_wm_state          = XInternAtom(dpy, "_NET_WM_STATE", False);
    a_net_wm_window_type    = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE", False);
    a_net_wm_window_type_dialog = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_DIALOG", False);
    a_net_wm_window_type_normal = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_NORMAL", False);
    a_net_wm_state_fullscreen  = XInternAtom(dpy, "_NET_WM_STATE_FULLSCREEN", False);
    a_net_wm_state_max_v       = XInternAtom(dpy, "_NET_WM_STATE_MAXIMIZED_VERT", False);
    a_net_wm_state_max_h       = XInternAtom(dpy, "_NET_WM_STATE_MAXIMIZED_HORZ", False);
    a_net_wm_state_modal       = XInternAtom(dpy, "_NET_WM_STATE_MODAL", False);
    a_net_active_window        = XInternAtom(dpy, "_NET_ACTIVE_WINDOW", False);
}

// Synthetic ConfigureNotify sent to our own window, mimicking a client
// "confirming" a geometry it was told about. We select StructureNotify so our
// own event loop counts it (and the WM, if it forwards, also sees it).
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

// Geometry variations: normal, degenerate 0x0, huge, tiny, etc.
static const int geos[][4] = {
    { 100, 100, 300, 200 },
    { 0, 0, 1, 1 },          // degenerate (X rejects 0x0; 1x1 is the smallest)
    { 50, 50, 65000, 65000 }, // huge (CARD16 cap is 65535)
    { 10, 10, 10, 10 },
    { 200, 200, 640, 480 },
    { 0, 0, 1, 1 },          // near-degenerate
    { 300, 50, 5000, 5000 }, // huge-ish
    { 0, 0, 400, 300 },
};
#define NGEOS (int)(sizeof(geos) / sizeof(geos[0]))

static void set_state_prop(Window w, const Atom *atoms, int n) {
    XChangeProperty(dpy, w, a_net_wm_state, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)atoms, n);
}

static void set_wtype_prop(Window w, Atom t) {
    XChangeProperty(dpy, w, a_net_wm_window_type, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)&t, 1);
}

static Window make_window(int kind, Window parent) {
    XSetWindowAttributes wa;
    memset(&wa, 0, sizeof wa);
    Window w = XCreateWindow(dpy, root, 100, 100, 300, 200, 0,
                             CopyFromParent, InputOutput, CopyFromParent, 0, &wa);

    // Identify ourselves.
    XClassHint ch;
    char cn[32], cc[32];
    snprintf(cn, sizeof cn, "stress_%s", kind_name(kind));
    snprintf(cc, sizeof cc, "stress");
    ch.res_name = cn;
    ch.res_class = cc;
    XSetClassHint(dpy, w, &ch);
    XStoreName(dpy, w, cn);

    // Select the events we want to *count* on our own window.
    XSelectInput(dpy, w,
                 StructureNotifyMask | FocusChangeMask | PropertyChangeMask);

    // EWMH/ICCCM hinting so maverick classifies the window as the requested kind.
    switch (kind) {
        case KIND_FS: {
            Atom s[1] = { a_net_wm_state_fullscreen };
            set_state_prop(w, s, 1);
            break;
        }
        case KIND_FLOAT: {
            set_wtype_prop(w, a_net_wm_window_type_dialog);
            break;
        }
        case KIND_MAX: {
            Atom s[2] = { a_net_wm_state_max_v, a_net_wm_state_max_h };
            set_state_prop(w, s, 2);
            break;
        }
        case KIND_TRANS: {
            // Transient-for a (unmanaged) parent ⇒ modal/float child.
            XSetTransientForHint(dpy, w, parent);
            Atom s[1] = { a_net_wm_state_modal };
            set_state_prop(w, s, 1);
            break;
        }
        case KIND_TILE:
        default: {
            set_wtype_prop(w, a_net_wm_window_type_normal);
            break;
        }
    }
    return w;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s KIND ITERATIONS [DISPLAY]\n"
                        "  KIND ∈ tiled fullscreen float maximize transient\n",
                argv[0]);
        return 2;
    }

    int kind;
    if (!strcmp(argv[1], "tiled"))        kind = KIND_TILE;
    else if (!strcmp(argv[1], "fullscreen")) kind = KIND_FS;
    else if (!strcmp(argv[1], "float"))   kind = KIND_FLOAT;
    else if (!strcmp(argv[1], "maximize")) kind = KIND_MAX;
    else if (!strcmp(argv[1], "transient")) kind = KIND_TRANS;
    else { fprintf(stderr, "stress: unknown kind '%s'\n", argv[1]); return 2; }

    long iters = atol(argv[2]);
    if (iters <= 0) iters = 1;

    if (argc >= 4) {
        setenv("DISPLAY", argv[3], 1);
    }
    dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "stress: cannot open display\n"); return 1; }
    root = DefaultRootWindow(dpy);
    intern_atoms();

    // A dummy unmanaged parent for the transient/modal case.
    Window parent = XCreateWindow(dpy, root, 0, 0, 10, 10, 0,
                                  CopyFromParent, InputOutput, CopyFromParent, 0, NULL);

    long total_cfg_req = 0, total_cfg_notify = 0, total_focus_in = 0,
         total_focus_out = 0, total_prop_state = 0, total_active = 0,
         total_resize = 0, total_map = 0, total_unmap = 0,
         total_remap = 0, total_destroy = 0;

    for (long i = 0; i < iters; i++) {
        Window w = make_window(kind, parent);

        // 1. Map
        XMapWindow(dpy, w);
        c_map++;
        XFlush(dpy);

        // 2/3/4/5. Churn configure requests, synthetic notifies, active-window
        // messages and resizes with varying (incl. degenerate/huge) geometry.
        for (int g = 0; g < NGEOS; g++) {
            int x = geos[g][0], y = geos[g][1], ww = geos[g][2], hh = geos[g][3];

            // 2. ConfigureRequest (client → WM). X rejects 0 or >65535 sizes,
            // so clamp to the smallest legal degenerate (1) / 65000 cap.
            int cw = ww > 0 ? (ww > 65000 ? 65000 : ww) : 1;
            int ch = hh > 0 ? (hh > 65000 ? 65000 : hh) : 1;
            XMoveResizeWindow(dpy, w, x, y, cw, ch);
            c_cfg_req++;

            // 3. synthetic ConfigureNotify (client "confirms" a geometry)
            send_synthetic_configure(dpy ? w : w, x, y, ww, hh);

            // 5. Resize (a further ConfigureRequest variant)
            XResizeWindow(dpy, w, ww > 0 ? ww : 1, hh > 0 ? hh : 1);
            c_resize++;

            // 4. _NET_ACTIVE_WINDOW ClientMessage (focus grab attempt)
            send_active_window(w);
            c_active++;

            XFlush(dpy);
        }

        drain(); // observe what the WM did so far

        // 6. Unmap
        XUnmapWindow(dpy, w);
        c_unmap++;
        XFlush(dpy);

        // 7. Remap
        XMapWindow(dpy, w);
        c_remap++;
        XFlush(dpy);

        // 8. Destroy
        XDestroyWindow(dpy, w);
        c_destroy++;
        XFlush(dpy);

        drain(); // final count for this iteration's inbound events

        if ((i + 1) % 50 == 0) {
            fprintf(stderr, "[stress:%s] iter %ld done (cfg_notify=%ld "
                            "focus_in=%ld focus_out=%ld prop_state=%ld)\n",
                    kind_name(kind), i + 1, c_cfg_notify, c_focus_in,
                    c_focus_out, c_prop_state);
            fflush(stderr);
        }
    }

    // Tally
    total_cfg_req = c_cfg_req;
    total_cfg_notify = c_cfg_notify;
    total_focus_in = c_focus_in;
    total_focus_out = c_focus_out;
    total_prop_state = c_prop_state;
    total_active = c_active;
    total_resize = c_resize;
    total_map = c_map;
    total_unmap = c_unmap;
    total_remap = c_remap;
    total_destroy = c_destroy;

    printf("STRESS_RESULT kind=%s iters=%ld\n", kind_name(kind), iters);
    printf("  map=%ld unmap=%ld remap=%ld destroy=%ld\n", total_map, total_unmap, total_remap, total_destroy);
    printf("  configure_request=%ld resize=%ld active_window_msg=%ld\n",
           total_cfg_req, total_resize, total_active);
    printf("  configure_notify_recv=%ld focus_in=%ld focus_out=%ld state_prop_change=%ld\n",
           total_cfg_notify, total_focus_in, total_focus_out, total_prop_state);
    printf("STRESS_DONE\n");
    fflush(stdout);

    XCloseDisplay(dpy);
    return 0;
}
