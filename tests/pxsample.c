// Pixel-sampling assertion primitive for the Maverick partial-redraw harness.
//
// Reads a rectangle of the Composite overlay window (the actual composited
// output) and checks how many pixels match an expected colour within a
// per-channel tolerance. Exit 0 = match, 1 = mismatch / error. Prints a
// one-line summary so the harness can assert "no residue".
//
// Usage: pxsample X Y W H EXPECTED_HEX [TOL]
//   X Y W H       rectangle on the composited screen (overlay) to sample
//   EXPECTED_HEX   0xRRGGBB expected colour
//   TOL            per-channel tolerance (default 28)
//
// Colours are decoded through the root visual's RGB masks, so the comparison
// is independent of XImage byte order.

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/Xcomposite.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int chan_shift(unsigned long mask) {
    int s = 0;
    while (mask && !(mask & 1)) { mask >>= 1; s++; }
    return s;
}

static void decode(unsigned long pix, Visual *v, int *r, int *g, int *b) {
    int rs = chan_shift(v->red_mask), gs = chan_shift(v->green_mask), bs = chan_shift(v->blue_mask);
    int rmax = (v->red_mask >> rs), gmax = (v->green_mask >> gs), bmax = (v->blue_mask >> bs);
    *r = (int)(((pix & v->red_mask) >> rs) * 255 / rmax);
    *g = (int)(((pix & v->green_mask) >> gs) * 255 / gmax);
    *b = (int)(((pix & v->blue_mask) >> bs) * 255 / bmax);
}

int main(int argc, char **argv) {
    if (argc < 6) { fprintf(stderr, "usage: pxsample X Y W H HEX [TOL]\n"); return 1; }
    int x = atoi(argv[1]), y = atoi(argv[2]), w = atoi(argv[3]), h = atoi(argv[4]);
    unsigned long exp = strtoul(argv[5], 0, 16);
    int sample_root = 0;
    int tol = 28;
    if (argc > 6) {
        if (strcmp(argv[6], "--root") == 0) sample_root = 1;
        else tol = atoi(argv[6]);
    }

    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "pxsample: no display\n"); return 1; }
    int scr = DefaultScreen(d);
    Visual *vis = DefaultVisual(d, scr);

    int er, eg, eb;
    decode(exp, vis, &er, &eg, &eb);

    Window root = RootWindow(d, scr);
    Window target = sample_root ? root : XCompositeGetOverlayWindow(d, root);
    XImage *img = XGetImage(d, target, x, y, w, h, AllPlanes, ZPixmap);
    if (!img) { fprintf(stderr, "pxsample: XGetImage failed\n"); return 1; }

    long total = 0, match = 0;
    int maxdr = 0, maxdg = 0, maxdb = 0;
    int first_r = -1, first_g = -1, first_b = -1;
    for (int iy = 0; iy < h; iy++) {
        for (int ix = 0; ix < w; ix++) {
            unsigned long pix = XGetPixel(img, ix, iy);
            int r, g, b;
            decode(pix, vis, &r, &g, &b);
            if (first_r < 0) { first_r = r; first_g = g; first_b = b; }
            int dr = abs(r - er), dg = abs(g - eg), db = abs(b - eb);
            total++;
            if (dr <= tol && dg <= tol && db <= tol) match++;
            else { if (dr > maxdr) maxdr = dr; if (dg > maxdg) maxdg = dg; if (db > maxdb) maxdb = db; }
        }
    }
    XDestroyImage(img);
    if (!sample_root) XCompositeReleaseOverlayWindow(d, root);

    long mism = total - match;
    if (mism == 0) {
        printf("OK   pxsample %dx%d@(%d,%d) exp=0x%06lx match=%ld/%ld\n", w, h, x, y, exp, match, total);
        return 0;
    }
    printf("FAIL pxsample %dx%d@(%d,%d) exp=0x%06lx got=0x%02x%02x%02x match=%ld/%ld worst=+%d,+%d,+%d\n",
           w, h, x, y, exp, first_r, first_g, first_b, match, total, maxdr, maxdg, maxdb);
    return 1;
}
