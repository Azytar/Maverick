#include <X11/Xlib.h>
#include <X11/XKBlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <poll.h>

static Display *d;
static Window root;
static Atom desktop;
static int grab_error;


static int error_handler(Display *display, XErrorEvent *e) {
    (void)display;
    grab_error = e->error_code;
    return 0;
}

static XEvent event(void) {
    XEvent e;
    if (!XPending(d)) {
        struct pollfd fd = {ConnectionNumber(d), POLLIN, 0};
        if (poll(&fd, 1, 5000) <= 0) {
            fprintf(stderr, "event barrier timed out\n");
            exit(2);
        }
    }
    XNextEvent(d, &e);
    return e;
}

static unsigned long current(void) {
    Atom type;
    int format;
    unsigned long count, left, value = 999;
    unsigned char *data = NULL;
    XGetWindowProperty(d, root, desktop, 0, 1, False, AnyPropertyType,
                       &type, &format, &count, &left, &data);
    if (count && format == 32) value = *(unsigned long *)data;
    if (data) XFree(data);
    return value;
}

static void state(void) {
    XkbStateRec s;
    XkbGetState(d, XkbUseCoreKbd, &s);
    printf("STATE group=%u locked_group=%u base_group=%d mods=%u locked_mods=%u\n",
           s.group, s.locked_group, s.base_group, s.mods, s.locked_mods);
}

static void key(unsigned code, int down) {
    XTestFakeKeyEvent(d, code, down, CurrentTime);
}

static void chord(unsigned code, unsigned mask) {
    const KeySym modifiers[] = {XK_Shift_L, XK_Control_L, XK_Alt_L, XK_Super_L};
    const unsigned masks[] = {ShiftMask, ControlMask, Mod1Mask, Mod4Mask};
    for (int i = 0; i < 4; ++i)
        if (mask & masks[i]) key(XKeysymToKeycode(d, modifiers[i]), True);
    key(code, True);
    key(code, False);
    for (int i = 3; i >= 0; --i)
        if (mask & masks[i]) key(XKeysymToKeycode(d, modifiers[i]), False);
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;
    setvbuf(stdout, NULL, _IOLBF, 0);
    d = XOpenDisplay(NULL);
    if (!d) return 2;
    root = DefaultRootWindow(d);
    desktop = XInternAtom(d, "_NET_CURRENT_DESKTOP", False);
    XSelectInput(d, root, PropertyChangeMask | KeyPressMask);
    XSetErrorHandler(error_handler);
    XSync(d, False);
    puts("READY");
    char command[128];
    while (fgets(command, sizeof command, stdin)) {
        unsigned code, mask, group, locks;
        if (sscanf(command, "group %u", &group) == 1) {
            XkbLockGroup(d, XkbUseCoreKbd, group);
            XSync(d, False);
            state();
        } else if (sscanf(command, "lock %u", &locks) == 1) {
            XkbLockModifiers(d, XkbUseCoreKbd, 255, locks);
            XSync(d, False);
            state();
        } else if (sscanf(command, "grab %u %u", &code, &mask) == 2) {
            grab_error = 0;
            XGrabKey(d, code, mask, root, True, GrabModeAsync, GrabModeAsync);
            XSync(d, False);
            int result = grab_error;
            if (!result) XUngrabKey(d, code, mask, root);
            XSync(d, False);
            printf("GRAB code=%u mask=%u error=%d\n", code, mask, result);
        } else if (sscanf(command, "test %u %u", &code, &mask) == 2) {
            XEvent reset = {0};
            reset.xclient.type = ClientMessage;
            reset.xclient.window = root;
            reset.xclient.message_type = desktop;
            reset.xclient.format = 32;
            reset.xclient.data.l[0] = 0;
            XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &reset);
            XFlush(d);
            while (current() != 0) event();
            XSync(d, False);
            while (XPending(d)) event();
            XSetInputFocus(d, root, RevertToPointerRoot, CurrentTime);
            state();
            XkbStateRec s;
            XkbGetState(d, XkbUseCoreKbd, &s);
            XkbDescPtr map = XkbGetMap(d, XkbAllClientInfoMask, XkbUseCoreKbd);
            printf("INJECT code=%u mask=%u event_state=%u primary=%lx active=%lx\n",
                   code, mask, XkbBuildCoreState(s.mods | mask, s.group),
                   XkbKeySymEntry(map, code, 0, 0), XkbKeySymEntry(map, code, 0, s.group));
            XkbFreeKeyboard(map, XkbAllComponentsMask, True);
            chord(code, mask);
            XFlush(d);
            for (;;) {
                XEvent e = event();
                if (e.type == KeyPress) {
                    printf("CLIENT_KEY code=%u state=%u\n", e.xkey.keycode, e.xkey.state);
                    if (e.xkey.keycode == code) break;
                }
                if (e.type == PropertyNotify && e.xproperty.atom == desktop) {
                    unsigned long value = current();
                    printf("DESKTOP %lu\n", value);
                    if (value != 0) break;
                }
            }
            XkbLockModifiers(d, XkbUseCoreKbd, 255, 0);
            XSync(d, False);
            puts("DONE");
        } else if (!strncmp(command, "map", 3)) {
            int min, max, per;
            XDisplayKeycodes(d, &min, &max);
            KeySym *map = XGetKeyboardMapping(d, min, max - min + 1, &per);
            printf("MAP kpk=%d", per);
            for (int c = 24; c <= 54; ++c) {
                printf(" code%d=", c);
                for (int k = 0; k < per; ++k)
                    printf("%s%lx", k ? "," : "", map[(c - min) * per + k]);
            }
            puts("");
            XFree(map);
            state();
        } else if (!strncmp(command, "state", 5)) state();
        else break;
    }
    XCloseDisplay(d);
    return 0;
}
