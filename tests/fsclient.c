// Minimal X11 client for forensic fullscreen/stale-frame harness.
//
// Usage: fsclient [X Y W H COLOR]
//
// Behavior:
// - Creates an override-redirect window with solid colour.
// - Repaints continuously in a small moving inner dot to guarantee XDamage.
// - SIGUSR1 toggles _NET_WM_STATE_FULLSCREEN on the client window.
// - SIGTERM exits cleanly.
// - Prints WINID=0x... on stderr for harness parsing.
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

static volatile sig_atomic_t g_fullscreen;

static void on_sigusr1(int sig) {
  (void)sig;
  g_fullscreen = !g_fullscreen;
}

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

int main(int argc, char **argv) {
  const char *dpy_name = getenv("DISPLAY");
  Display *d = XOpenDisplay(dpy_name);
  if (!d) { fprintf(stderr, "fsclient: no display %s\n", dpy_name ? dpy_name : ""); return 1; }
  int scr = DefaultScreen(d);
  int x = argc > 1 ? atoi(argv[1]) : 100;
  int y = argc > 2 ? atoi(argv[2]) : 100;
  int w = argc > 3 ? atoi(argv[3]) : 400;
  int h = argc > 4 ? atoi(argv[4]) : 300;
  unsigned long base = alloc_pixel(d, argc > 5 ? strtoul(argv[5], 0, 16) : 0xff3366);
  unsigned long dot = 0xffffff - base;

  XSetWindowAttributes wa;
  wa.override_redirect = True;
  Window win = XCreateWindow(d, RootWindow(d, scr), x, y, w, h, 0,
                             CopyFromParent, InputOutput, CopyFromParent,
                             CWOverrideRedirect, &wa);
  XSelectInput(d, win, ExposureMask | StructureNotifyMask);
  XClassHint ch; char n[] = "fsclient", c[] = "fsclient";
  ch.res_name = n; ch.res_class = c; XSetClassHint(d, win, &ch);
  GC gc = XCreateGC(d, win, 0, 0);
  XMapWindow(d, win);
  XFlush(d);
  fprintf(stderr, "WINID=0x%lx\n", (unsigned long) win);
  fflush(stderr);

  struct sigaction sa = {0};
  sa.sa_handler = on_sigusr1;
  sigemptyset(&sa.sa_mask);
  sa.sa_flags = 0;
  sigaction(SIGUSR1, &sa, NULL);

  int dot_x = 0, dot_y = 0, ppx = -100, ppy = -100, frame = 0;
  Atom net_wm_state = XInternAtom(d, "_NET_WM_STATE", False);
  Atom net_wm_state_fullscreen = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
  Atom net_wm_name = XInternAtom(d, "_NET_WM_NAME", False);
  Atom utf8_string = XInternAtom(d, "UTF8_STRING", False);
  Atom mav_test_frame = XInternAtom(d, "_MAVERICK_TEST_FRAME", False);
  while (1) {
    XEvent ev;
    while (XPending(d)) {
      XNextEvent(d, &ev);
      if (ev.type == ClientMessage) {
        XClientMessageEvent *cm = (XClientMessageEvent *)&ev;
        if (cm->message_type == net_wm_state) {
          long action = cm->data.l[0];
          long state = cm->data.l[1];
          long source = cm->data.l[2];
          (void)action; (void)state; (void)source;
        }
      }
    }

    if (g_fullscreen) {
      XClientMessageEvent cm = {0};
      cm.type = ClientMessage;
      cm.window = win;
      cm.message_type = net_wm_state;
      cm.format = 32;
      cm.data.l[0] = 1; /* _NET_WM_STATE_ADD */
      cm.data.l[1] = net_wm_state_fullscreen;
      cm.data.l[2] = 0;
      cm.data.l[3] = 1; /* source indication */
      cm.data.l[4] = 0;
      XSendEvent(d, DefaultRootWindow(d), False,
                 SubstructureNotifyMask | SubstructureRedirectMask,
                 (XEvent *)&cm);
      g_fullscreen = 0;
    }

    XSetForeground(d, gc, base);
    XFillRectangle(d, win, gc, 0, 0, w, h);
    if (ppx >= 0) {
      XSetForeground(d, gc, base);
      XFillRectangle(d, win, gc, ppx, ppy, 50, 50);
    }
    dot_x = (frame * 13) % (w - 50);
    dot_y = (frame * 17) % (h - 50);
    XSetForeground(d, gc, dot);
    XFillRectangle(d, win, gc, dot_x, dot_y, 50, 50);
    ppx = dot_x; ppy = dot_y;

    char title[64];
    snprintf(title, sizeof(title), "fsclient frame=%d", frame);
    XStoreName(d, win, title);
    XChangeProperty(d, win, net_wm_name, utf8_string, 8,
                    PropModeReplace, (unsigned char *)title, strlen(title));
    long frame_val = frame;
    XChangeProperty(d, win, mav_test_frame, XA_CARDINAL, 32,
                    PropModeReplace, (unsigned char *)&frame_val, 1);

    const char *state_path = getenv("MAVERICK_TEST_STATE");
    if (state_path && *state_path) {
        char tmp[256];
        snprintf(tmp, sizeof(tmp), "%s.tmp.%d", state_path, getpid());
        char buf[64];
        int n = -1;
        struct timeval tv;
        if (gettimeofday(&tv, NULL) == 0) {
            long time_us = (long)tv.tv_sec * 1000000L + tv.tv_usec;
            n = snprintf(buf, sizeof(buf), "%d %ld\n", frame, time_us);
        } else {
            n = snprintf(buf, sizeof(buf), "%d 0\n", frame);
        }
        if (n > 0 && n < (int)sizeof(buf)) {
            int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
            if (fd >= 0) {
                (void)write(fd, buf, (unsigned)n);
                (void)close(fd);
                rename(tmp, state_path);
            }
        }
    }

    XFlush(d);
    frame++;
    usleep(16000);
  }
  return 0;
}
