/* Query displayed cursor pixels independently of the window manager. */
#include <X11/Xlib.h>
#include <X11/cursorfont.h>
#include <X11/extensions/Xfixes.h>
#include <inttypes.h>
#include <stdio.h>
#include <string.h>

static void sample(Display *display) {
    XSync(display, False);
    XFixesCursorImage *image = XFixesGetCursorImage(display);
    if (!image) return;
    unsigned visible = 0;
    uint64_t hash = UINT64_C(14695981039346656037);
    for (unsigned i = 0; i < (unsigned)image->width * image->height; i++) {
        uint32_t pixel = (uint32_t)image->pixels[i];
        if (pixel >> 24) visible++;
        hash = (hash ^ pixel) * UINT64_C(1099511628211);
    }
    printf("%u %u %u %" PRIu64 "\n", image->width, image->height, visible, hash);
    XFree(image);
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (!display) return 1;
    int event, error;
    if (!XFixesQueryExtension(display, &event, &error)) return 2;
    Window root = DefaultRootWindow(display);
    Window client = None;
    setvbuf(stdout, NULL, _IOLBF, 0);
    XWarpPointer(display, None, root, 0, 0, 0, 0, 400, 300);
    sample(display);
    char line[32];
    while (fgets(line, sizeof(line), stdin)) {
        if (!strcmp(line, "blank\n")) {
            char bits[1] = {0};
            Pixmap bitmap = XCreateBitmapFromData(display, root, bits, 1, 1);
            XColor color = {0};
            Cursor cursor = XCreatePixmapCursor(display, bitmap, bitmap, &color, &color, 0, 0);
            XDefineCursor(display, root, cursor);
            XFreeCursor(display, cursor);
            XFreePixmap(display, bitmap);
        } else if (!strcmp(line, "client\n")) {
            client = XCreateSimpleWindow(display, root, 0, 0, 400, 300, 0, 0, 0);
            Cursor cursor = XCreateFontCursor(display, XC_watch);
            XDefineCursor(display, client, cursor);
            XFreeCursor(display, cursor);
            XStoreName(display, client, "cursor-probe");
            XMapWindow(display, client);
        } else if (!strcmp(line, "destroy\n")) {
            XDestroyWindow(display, client);
            client = None;
        } else if (!strcmp(line, "quit\n")) break;
        else if (strcmp(line, "sample\n")) return 3;
        sample(display);
    }
    XCloseDisplay(display);
    return 0;
}
