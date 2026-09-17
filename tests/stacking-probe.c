// Cooperative X11 stacking probe. Used only on the isolated Xvfb test server.
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    Display *d = XOpenDisplay(NULL);
    if (!d) return 1;
    Window root = DefaultRootWindow(d), dock = None, peer = None;
    Window win = XCreateSimpleWindow(d, root, 0, 0, 400, 300, 0, 0, 0xffffff);
    XStoreName(d, win, "stacking-probe");
    XMapWindow(d, win);
    XSync(d, False);
    printf("%lu\n", win); fflush(stdout);
    char cmd[64];
    while (fgets(cmd, sizeof(cmd), stdin)) {
        if (!strncmp(cmd, "dock", 4)) {
            dock = XCreateSimpleWindow(d, root, 0, 0, 800, 24, 0, 0, 0xff0000);
            Atom type = XInternAtom(d, "_NET_WM_WINDOW_TYPE_DOCK", False);
            unsigned long strut[12] = {0, 0, 24, 0, 0, 0, 0, 0, 0, 799, 0, 0};
            XChangeProperty(d, dock, XInternAtom(d, "_NET_WM_WINDOW_TYPE", False),
                            XA_ATOM, 32, PropModeReplace, (unsigned char *)&type, 1);
            XChangeProperty(d, dock, XInternAtom(d, "_NET_WM_STRUT_PARTIAL", False),
                            XA_CARDINAL, 32, PropModeReplace, (unsigned char *)strut, 12);
            XMapWindow(d, dock);
        } else if (!strncmp(cmd, "remove", 6)) {
            if (dock) XDestroyWindow(d, dock);
            dock = None;
        } else if (!strncmp(cmd, "unpeer", 6)) {
            if (peer) XDestroyWindow(d, peer);
            peer = None;
        } else if (!strncmp(cmd, "fs", 2)) {
            XEvent e = {0};
            e.xclient.type = ClientMessage;
            e.xclient.window = win;
            e.xclient.message_type = XInternAtom(d, "_NET_WM_STATE", False);
            e.xclient.format = 32;
            e.xclient.data.l[0] = 1;
            e.xclient.data.l[1] = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
            e.xclient.data.l[3] = 1;
            XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e);
        } else if (!strncmp(cmd, "peer", 4)) {
            /* Map a second, ordinary managed tile (no dock struts, no name
             * match needed: the harness tracks the returned id). */
            peer = XCreateSimpleWindow(d, root, 0, 0, 300, 200, 0, 0, 0x00ff00);
            XStoreName(d, peer, "stacking-peer");
            XMapWindow(d, peer);
        } else if (!strncmp(cmd, "check", 5)) {
            Window r, parent, *children = NULL;
            unsigned int count = 0;
            int wi = -1, di = -1;
            XWindowAttributes a;
            if (!XQueryTree(d, root, &r, &parent, &children, &count)) return 2;
            for (unsigned int i = 0; i < count; i++) {
                if (children[i] == win) wi = (int)i;
                if (children[i] == dock) di = (int)i;
            }
            if (children) XFree(children);
            XGetWindowAttributes(d, win, &a);
            printf("%d %d %d %d %d %d %d\n", wi, di, a.x, a.y,
                   a.width, a.height, a.border_width);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "pcheck", 6)) {
            /* Peer-tile regression report: "wi pi px py pw ph pbw".
             * XQueryTree lists bottom-to-top, so A above B == wi > pi. */
            Window r, parent, *children = NULL;
            unsigned int count = 0;
            int wi = -1, pi = -1;
            XWindowAttributes pa;
            if (!XQueryTree(d, root, &r, &parent, &children, &count)) return 2;
            for (unsigned int i = 0; i < count; i++) {
                if (children[i] == win) wi = (int)i;
                if (children[i] == peer) pi = (int)i;
            }
            if (children) XFree(children);
            int px = -1, py = -1;
            unsigned pw = 0, ph = 0, pbw = 0;
            if (peer && XGetWindowAttributes(d, peer, &pa)) {
                px = pa.x; py = pa.y; pw = (unsigned)pa.width;
                ph = (unsigned)pa.height; pbw = (unsigned)pa.border_width;
            }
            printf("%d %d %d %d %u %u %u\n", wi, pi, px, py, pw, ph, pbw);
            fflush(stdout);
            continue;
        } else if (!strncmp(cmd, "quit", 4)) {
            break;
        } else return 3;
        XSync(d, False);
        puts("ok"); fflush(stdout);
    }
    XCloseDisplay(d);
    return 0;
}
