/* Observe client events and sample the displayed image independently of the WM. */
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/Xcomposite.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <unistd.h>

struct client {
    Window win;
    unsigned long color;
    int width, height;
    unsigned resizes, configures, maps, unmaps, clicks;
};

static Display *display;
static struct client clients[8];
static unsigned count;
static void paint(Window win) {
    struct client *client = NULL;
    for (unsigned i = 0; i < count; i++) if (clients[i].win == win) client = &clients[i];
    if (!client) return;
    XWindowAttributes attr;
    XGetWindowAttributes(display, win, &attr);
    GC gc = XCreateGC(display, win, 0, NULL);
    XSetForeground(display, gc, client->color);
    XFillRectangle(display, win, gc, 0, 0, (unsigned)attr.width, (unsigned)attr.height);
    XSetForeground(display, gc, 0x00ff00);
    XFillRectangle(display, win, gc, 0, 0, (unsigned)attr.width / 2, (unsigned)attr.height / 2);
    XFreeGC(display, gc);
}

static void drain(void) {
    while (XPending(display)) {
        XEvent event;
        XNextEvent(display, &event);
        for (unsigned i = 0; i < count; i++) {
            struct client *c = &clients[i];
            if (event.xany.window != c->win) continue;
            switch (event.type) {
            case ConfigureNotify:
                c->configures++;
                if (c->width != event.xconfigure.width || c->height != event.xconfigure.height)
                    c->resizes++;
                c->width = event.xconfigure.width;
                c->height = event.xconfigure.height;
                break;
            case MapNotify: c->maps++; break;
            case UnmapNotify: c->unmaps++; break;
            case ButtonPress: c->clicks++; break;
            case Expose:
                if (!event.xexpose.count) paint(c->win);
                break;
            default: break;
            }
        }
    }
}

static Window create_client(void) {
    if (count == sizeof(clients) / sizeof(clients[0])) return None;
    Window win = XCreateSimpleWindow(display, DefaultRootWindow(display), 40, 40, 400, 300, 0, 0, 0xffffff);
    XStoreName(display, win, "overview-probe");
    XClassHint hint = {"overview-probe", "overview-probe"};
    XSetClassHint(display, win, &hint);
    XSelectInput(display, win, StructureNotifyMask | ExposureMask | ButtonPressMask);
    clients[count++] = (struct client){.win = win, .color = 0xffffff, .width = 400, .height = 300};
    XMapWindow(display, win);
    XFlush(display);
    return win;
}

int main(void) {
    display = XOpenDisplay(NULL);
    if (!display) return 1;
    setvbuf(stdout, NULL, _IOLBF, 0);
    printf("%lu\n", create_client());
    for (;;) {
        fd_set readset;
        FD_ZERO(&readset);
        FD_SET(STDIN_FILENO, &readset);
        FD_SET(ConnectionNumber(display), &readset);
        XFlush(display);
        drain();
        if (select(ConnectionNumber(display) + 1, &readset, NULL, NULL, NULL) < 0)
            return 2;
        drain();
        if (!FD_ISSET(STDIN_FILENO, &readset)) continue;
        char line[128];
        if (!fgets(line, sizeof(line), stdin)) break;
        XSync(display, False);
        drain();
        unsigned i;
        int x, y;
        unsigned long value;
        if (sscanf(line, "stats %u", &i) == 1 && i < count) {
            XWindowAttributes attr;
            XGetWindowAttributes(display, clients[i].win, &attr);
            struct client *c = &clients[i];
            printf("%d %d %d %d %d %u %u %u %u %u\n", attr.x, attr.y, attr.width, attr.height,
                   attr.border_width, c->resizes, c->configures, c->maps, c->unmaps, c->clicks);
        } else if (!strncmp(line, "reset", 5)) {
            for (unsigned j = 0; j < count; j++) {
                clients[j].resizes = clients[j].configures = clients[j].maps = clients[j].unmaps = clients[j].clicks = 0;
            }
            puts("ok");
        } else if (!strncmp(line, "new", 3)) {
            printf("%lu\n", create_client());
        } else if (sscanf(line, "paintwin %u %lx", &i, &value) == 2 && i < count) {
            clients[i].color = value;
            paint(clients[i].win);
            XSync(display, False);
            puts("ok");
        } else if (sscanf(line, "resize %u %d %d", &i, &x, &y) == 3 && i < count && x > 0 && y > 0) {
            XResizeWindow(display, clients[i].win, (unsigned)x, (unsigned)y);
            XSync(display, False);
            puts("ok");
        } else if (sscanf(line, "paint %lx", &value) == 1) {
            for (unsigned j = 0; j < count; j++) {
                clients[j].color = value;
                if (clients[j].win) paint(clients[j].win);
            }
            XSync(display, False);
            puts("ok");
        } else if (sscanf(line, "pixel %d %d", &x, &y) == 2 || sscanf(line, "bounds %lx", &value) == 1) {
            int bounds = !strncmp(line, "bounds", 6);
            Window root = DefaultRootWindow(display);
            char selection[32];
            snprintf(selection, sizeof(selection), "_NET_WM_CM_S%d", DefaultScreen(display));
            int composited = XGetSelectionOwner(display, XInternAtom(display, selection, False)) != None;
            /* A compositor paints to the overlay; the redirected root image
             * is its input, not the image currently displayed on screen. */
            Drawable drawable = composited ? XCompositeGetOverlayWindow(display, root) : root;
            unsigned width = (unsigned)DisplayWidth(display, DefaultScreen(display));
            unsigned height = (unsigned)DisplayHeight(display, DefaultScreen(display));
            XImage *image = XGetImage(display, drawable, bounds ? 0 : x, bounds ? 0 : y,
                                     bounds ? width : 1, bounds ? height : 1, AllPlanes, ZPixmap);
            if (!image) return 3;
            if (!bounds) printf("%lu\n", XGetPixel(image, 0, 0));
            else {
                int left = (int)width, top = (int)height, right = -1, bottom = -1;
                for (unsigned iy = 0; iy < height; iy++) {
                    for (unsigned ix = 0; ix < width; ix++) {
                        if ((XGetPixel(image, (int)ix, (int)iy) & 0xffffff) != value) continue;
                        if ((int)ix < left) left = (int)ix;
                        if ((int)ix > right) right = (int)ix;
                        if ((int)iy < top) top = (int)iy;
                        if ((int)iy > bottom) bottom = (int)iy;
                    }
                }
                printf("%d %d %d %d\n", left, top, right - left + 1, bottom - top + 1);
            }
            XDestroyImage(image);
            if (composited) XCompositeReleaseOverlayWindow(display, root);
        } else if (!strncmp(line, "owner", 5)) {
            char selection[32];
            snprintf(selection, sizeof(selection), "_NET_WM_CM_S%d", DefaultScreen(display));
            printf("%lu\n", XGetSelectionOwner(display, XInternAtom(display, selection, False)));
        } else if (sscanf(line, "destroy %u", &i) == 1 && i < count) {
            XDestroyWindow(display, clients[i].win);
            clients[i].win = None;
            XSync(display, False);
            puts("ok");
        } else if (!strncmp(line, "quit", 4)) {
            break;
        } else return 4;
    }
    XCloseDisplay(display);
    return 0;
}
