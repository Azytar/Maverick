#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Own the Xlib ABI and connection in one process/thread. Python retains the
 * WM's IPC assertions; a command acknowledgement only fences this client's
 * requests to the X server, never claims the WM has processed them. */
static Display *display;
static Window root, windows[2];
static Atom active_atom;

static void fail(const char *message) {
    fprintf(stderr, "input-hint-probe: %s\n", message);
    exit(2);
}

static int xerror(Display *dpy, XErrorEvent *event) {
    char message[128];
    XGetErrorText(dpy, event->error_code, message, sizeof(message));
    fprintf(stderr, "input-hint-probe: %s request=%u window=%lx\n",
            message, event->request_code, event->resourceid);
    exit(2);
}

static void input(Window window, int value) {
    XWMHints hints = {0};
    hints.flags = value == 2 ? 0 : InputHint;
    hints.input = value == 1;
    if (!XSetWMHints(display, window, &hints)) fail("XSetWMHints failed");
}

static void snapshot(Window window) {
    Atom type;
    int format, revert;
    unsigned long count, remaining;
    unsigned char *data = NULL;
    Window focused, active = None;
    if (!XGetInputFocus(display, &focused, &revert)) fail("XGetInputFocus failed");
    int status = XGetWindowProperty(display, root, active_atom, 0, 1, False,
                                    AnyPropertyType, &type, &format, &count,
                                    &remaining, &data);
    if (status != Success || (type != None && (type != XA_WINDOW || format != 32 || count != 1
            || remaining || !data))) fail("invalid _NET_ACTIVE_WINDOW");
    if (type != None) active = *(unsigned long *)data;
    if (data) XFree(data);
    /* XGetWMHints handles Xlib's native-long representation of format 32. */
    XWMHints *hints = XGetWMHints(display, window);
    printf("{\"active\":");
    if (type == None) printf("null");
    else printf("%lu", active);
    printf(",\"focus\":%lu,\"root\":%lu,\"hints\":", focused, root);
    if (hints) {
        printf("[%s,%s]", (hints->flags & InputHint) ? "true" : "false",
               hints->input ? "true" : "false");
        XFree(hints);
    } else {
        printf("null");
    }
    puts("}");
}

int main(int argc, char **argv) {
    (void)argv;
    if (argc != 1) fail("no options supported");
    display = XOpenDisplay(NULL);
    if (!display) fail("cannot open display");
    XSetErrorHandler(xerror);
    root = DefaultRootWindow(display);
    active_atom = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    /* No stdio read-ahead: poll must see every pending command on stdin. */
    setvbuf(stdin, NULL, _IONBF, 0);
    setvbuf(stdout, NULL, _IOLBF, 0);
    puts("{\"ready\":true}");
    for (;;) {
        /* Bound draining so a noisy server cannot starve control commands. */
        for (int i = 0; i < 256 && XPending(display); i++) {
            XEvent event;
            XNextEvent(display, &event);
        }
        struct pollfd fds[] = {{STDIN_FILENO, POLLIN, 0},
                               {ConnectionNumber(display), POLLIN, 0}};
        if (poll(fds, 2, -1) < 0) {
            if (errno == EINTR) continue;
            fail("poll failed");
        }
        if (fds[1].revents & (POLLERR | POLLHUP | POLLNVAL)) fail("X connection lost");
        if (!(fds[0].revents & (POLLIN | POLLHUP))) continue;
        char line[128], command[16], index_text[16], value_text[16], extra;
        int value = 0;
        if (!fgets(line, sizeof(line), stdin)) break;
        if (!strchr(line, '\n')) fail("incomplete or oversized command");
        int fields = sscanf(line, "%15s %15s %15s %c", command, index_text, value_text, &extra);
        if (fields < 2 || (strcmp(index_text, "0") && strcmp(index_text, "1")))
            fail("invalid command/index");
        unsigned index = (unsigned)(index_text[0] - '0');
        int is_input = strcmp(command, "input") == 0;
        if (fields != (is_input ? 3 : 2)) fail("invalid command arity");
        if (is_input) {
            if (strlen(value_text) != 1 || value_text[0] < '0' || value_text[0] > '2')
                fail("invalid input model");
            value = value_text[0] - '0';
        }
        if (strcmp(command, "create") == 0) {
            if (windows[index]) fail("window already exists");
            Window win = XCreateSimpleWindow(display, root, 40, 40, 400, 300, 2, 0, 0xffffff);
            windows[index] = win;
            XClassHint class_hint = {index ? "beta" : "alpha", "probe"};
            XSetClassHint(display, win, &class_hint);
            XStoreName(display, win, class_hint.res_name);
            input(win, 0);
            XMapWindow(display, win);
            XSync(display, False);
            printf("{\"window\":%lu}\n", win);
            continue;
        }
        Window win = windows[index];
        if (!win) fail("window does not exist");
        if (is_input) {
            input(win, value);
        } else if (strcmp(command, "snapshot") == 0) {
            snapshot(win);
            continue;
        } else if (strcmp(command, "drop") == 0) {
            XDeleteProperty(display, win, XA_WM_HINTS);
        } else if (strcmp(command, "unmap") == 0) {
            XUnmapWindow(display, win);
        } else if (strcmp(command, "map") == 0) {
            XMapWindow(display, win);
        } else if (strcmp(command, "destroy") == 0) {
            XDestroyWindow(display, win);
            windows[index] = None;
        } else {
            fail("unknown command");
        }
        XSync(display, False);
        puts("{\"ok\":true}");
    }
    XCloseDisplay(display);
    return 0;
}
