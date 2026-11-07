#include <X11/Xlib.h>
int main(void) {
    Display *d=XOpenDisplay(0);
    if(!d) return 2;
    /* The preload guard must reject this before Xlib sends the request. */
    XSelectInput(d,DefaultRootWindow(d),SubstructureRedirectMask);
    XCloseDisplay(d);
    return 0;
}
