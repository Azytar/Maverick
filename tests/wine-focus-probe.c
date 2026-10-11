#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

// Wine-style focus probe: a managed "game" window plus an override-redirect
// "capture" window (Wine/Proton relative-mouse capture). Reports focus
// stability as JSON RESULT lines for tests/xvfb-wine-focus.py.
//
// While the client holds the X input focus on its capture window, the WM must
// leave it alone: no counter-SetInputFocus (no FocusIn flap on the game
// window) and no WM_TAKE_FOCUS protocol traffic to the game window (a spurious
// take-focus makes a LocallyActive client re-focus its frame, resetting the
// game's key state mid-play). Both are observable black-box: the probe selects
// FocusChange on the game window and reads every ClientMessage it receives.

#define ok(name, cond, fmt, ...) do { \
  printf("RESULT {\"name\": \"%s\", \"passed\": %s, \"detail\": \"", name, (cond)?"true":"false"); \
  printf(fmt, ##__VA_ARGS__); \
  printf("\"}\n"); fflush(stdout); } while(0)

static int flap_to_game = 0;

int main(void) {
  Display *d = XOpenDisplay(NULL);
  if (!d) { printf("RESULT {\"name\": \"open\", \"passed\": false, \"detail\": \"no display\"}\n"); return 2; }
  int s = DefaultScreen(d);
  Window root = RootWindow(d, s);

  Window game = XCreateSimpleWindow(d, root, 100, 100, 800, 600, 2,
      BlackPixel(d, s), WhitePixel(d, s));
  XStoreName(d, game, "wine-game");
  XClassHint *ch = XAllocClassHint();
  ch->res_name = "wine-game"; ch->res_class = "Wine";
  XSetClassHint(d, game, ch);
  XFree(ch);
  XWMHints *h = XAllocWMHints();
  h->flags = InputHint; h->input = True;
  XSetWMHints(d, game, h);
  XFree(h);
  Atom take = XInternAtom(d, "WM_TAKE_FOCUS", False);
  XSetWMProtocols(d, game, &take, 1);
  XSelectInput(d, game, FocusChangeMask | StructureNotifyMask);
  XMapWindow(d, game);
  XSync(d, False);

  Window f; int r;
  // Gate on viewability first (the server has mapped), then poll for focus:
  // under load the manage+focus round trip can take longer than any one
  // hard-coded nap, and sampling focus before the map is even processed can
  // only report PointerRoot.
  for (int i = 0; i < 100; i++) {
    XWindowAttributes at;
    if (XGetWindowAttributes(d, game, &at) && at.map_state == IsViewable) break;
    usleep(100000);
  }
  // Poll (not a fixed sleep): under load the manage+focus round trip can take
  // longer than any one hard-coded nap.
  int mapped = 0;
  for (int i = 0; i < 100; i++) {
    XGetInputFocus(d, &f, &r);
    if (f == game) { mapped = 1; break; }
    usleep(100000);
  }
  XGetInputFocus(d, &f, &r);
  ok("game-gets-focus-on-map", mapped, "focus=%lx game=%lx", (unsigned long)f, (unsigned long)game);

  Atom protoA = XInternAtom(d, "WM_PROTOCOLS", False);
  Atom takeA = XInternAtom(d, "WM_TAKE_FOCUS", False);

  XSetWindowAttributes a;
  a.override_redirect = True;
  Window cap = XCreateWindow(d, root, 120, 120, 400, 300, 0, CopyFromParent,
      InputOutput, CopyFromParent, CWOverrideRedirect, &a);
  XStoreName(d, cap, "wine-capture");
  XSelectInput(d, cap, FocusChangeMask);
  XMapWindow(d, cap);
  XSync(d, False);
  usleep(300000);
  // Drain anything queued so far (notably the map-time FocusIn and the
  // map-time WM_TAKE_FOCUS on the game window): only protocol traffic and
  // focus events generated while the capture below is held may count.
  while (XPending(d)) {
    XEvent e;
    XNextEvent(d, &e);
  }
  // The game captures the input the way Wine/Proton does for relative mouse:
  // the X focus moves to the game's own override-redirect window. Hold it
  // there for ~3s with realistic pointer motion (boundary-crossing warps, so
  // the server keeps delivering EnterNotify with fresh timestamps) and one
  // mid-hold re-capture, the way a game re-asserts its capture after every
  // overlay dance or mode switch.
  XSetInputFocus(d, cap, RevertToParent, CurrentTime);
  XSync(d, False);
  int takefocus_to_game = 0;
  int stable = 1;
  for (int i = 0; i < 300; i++) {
    if (i == 150) {
      XSetInputFocus(d, cap, RevertToParent, CurrentTime);
      XSync(d, False);
    }
    if (i % 20 == 0) {
      XWarpPointer(d, None, root, 0, 0, 0, 0, 5, 5);
      XSync(d, False);
    } else if (i % 20 == 10) {
      XWarpPointer(d, None, game, 0, 0, 0, 0, 400 + (i % 3) * 17, 300);
      XSync(d, False);
    }
    while (XPending(d)) {
      XEvent e;
      XNextEvent(d, &e);
      if (e.type == FocusIn && e.xfocus.window == game && e.xfocus.mode == NotifyNormal) {
        flap_to_game = 1;
      }
      if (e.type == FocusOut && e.xfocus.window == cap && e.xfocus.mode == NotifyNormal) {
        flap_to_game = 1;
      }
      if (e.type == ClientMessage && e.xclient.message_type == protoA
          && (Atom)e.xclient.data.l[0] == takeA && e.xclient.window == game) {
        takefocus_to_game++;
      }
    }
    XGetInputFocus(d, &f, &r);
    if (f != cap) { stable = 0; }
    usleep(10000);
  }
  XGetInputFocus(d, &f, &r);
  ok("or-capture-keeps-focus", f == cap && stable, "focus=%lx cap=%lx game=%lx stable=%d",
      (unsigned long)f, (unsigned long)cap, (unsigned long)game, stable);
  ok("no-focus-flap-to-game", !flap_to_game, "flap_to_game=%d", flap_to_game);
  // The discriminator: every focus repair re-asserts the logical window with
  // set_input_focus *and* a WM_TAKE_FOCUS message. A WM that fights the
  // capture sends take-focus traffic to the game window while the capture is
  // held (observed: pre-fix builds do); a correct one sends none — its
  // logical selection never moved, so there is nothing to re-assert.
  ok("no-take-focus-while-captured", takefocus_to_game == 0, "takefocus_to_game=%d",
      takefocus_to_game);

  // The capture window goes away. Like Wine releasing a capture, point the
  // focus back at the game window and check it sticks: the WM's logical focus
  // never moved, so no repair should fight this re-focus either.
  XDestroyWindow(d, cap);
  XSync(d, False);
  usleep(300000);
  while (XPending(d)) {
    XEvent e;
    XNextEvent(d, &e);
  }
  XSetInputFocus(d, game, RevertToParent, CurrentTime);
  XSync(d, False);
  // Wait for the re-focus to land, then require it to stick: every sample
  // from the first game sample on must read the game window. (Transient
  // server-side revert pairs around PointerRoot while the destroy settles are
  // not WM steals, so this checks sampled stability rather than raw events.)
  int back = 0, landed = 0;
  for (int i = 0; i < 300; i++) {
    while (XPending(d)) {
      XEvent e;
      XNextEvent(d, &e);
    }
    XGetInputFocus(d, &f, &r);
    if (f == game) { landed = 1; }
    else if (landed) { landed = 2; break; }
    usleep(10000);
  }
  back = (landed == 1);
  XGetInputFocus(d, &f, &r);
  ok("focus-returns-to-game", back, "focus=%lx game=%lx", (unsigned long)f, (unsigned long)game);

  // A Globally Active client must answer using the timestamp in the offer.
  Window game2 = XCreateSimpleWindow(d, root, 60, 60, 640, 480, 2,
      BlackPixel(d, s), WhitePixel(d, s));
  XStoreName(d, game2, "wine-game-ga");
  XClassHint *ch2 = XAllocClassHint();
  ch2->res_name = "wine-game-ga"; ch2->res_class = "Wine";
  XSetClassHint(d, game2, ch2);
  XFree(ch2);
  XWMHints *h2 = XAllocWMHints();
  h2->flags = InputHint; h2->input = False;
  XSetWMHints(d, game2, h2);
  XFree(h2);
  XSetWMProtocols(d, game2, &take, 1);
  XSelectInput(d, game2, FocusChangeMask | KeyPressMask | StructureNotifyMask);
  XMapWindow(d, game2);
  XSync(d, False);
  // Poll for the offer the manage-time focus sends; count (never blindly
  // drain) from the very first drain so a fast WM's immediate offer cannot be
  // thrown away before the poll starts. A slow WM under load may take
  // seconds to manage, so poll generously.
  int offered = 0, extra_takefocus = 0;
  Time offered_time = CurrentTime;
  for (int i = 0; i < 1000 && !offered; i++) {
    while (XPending(d)) {
      XEvent e;
      XNextEvent(d, &e);
      if (e.type == ClientMessage && e.xclient.message_type == protoA
          && (Atom)e.xclient.data.l[0] == takeA && e.xclient.window == game2) {
        offered = 1;
        offered_time = (Time)e.xclient.data.l[1];
      }
    }
    if (!offered) usleep(10000);
  }
  ok("globally-active-take-focus-offered", offered, "offered=%d", offered);
  ok("globally-active-valid-timestamp", offered_time != CurrentTime,
      "timestamp=%lu", offered_time);
  // The X focus must be on the root: it must never be parked on a window
  // that declared it does not want it, nor left wherever it happened to be.
  // Poll: the offer above proves focus() ran, but its SetInputFocus request
  // may still be in flight to the server.
  int on_root = 0;
  for (int i = 0; i < 300 && !on_root; i++) {
    XGetInputFocus(d, &f, &r);
    if (f == root) { on_root = 1; break; }
    usleep(10000);
  }
  XGetInputFocus(d, &f, &r);
  ok("globally-active-x-focus-on-root", on_root, "focus=%lx root=%lx",
      (unsigned long)f, (unsigned long)root);
  // The client answers take-focus the way Wine does and takes the focus
  // itself. It must stick, with no further protocol storm.
  XSetInputFocus(d, game2, RevertToParent, offered_time);
  XSync(d, False);
  int ga_stable = 0, ga_landed = 0;
  for (int i = 0; i < 150; i++) {
    while (XPending(d)) {
      XEvent e;
      XNextEvent(d, &e);
      if (e.type == ClientMessage && e.xclient.message_type == protoA
          && (Atom)e.xclient.data.l[0] == takeA && e.xclient.window == game2) {
        extra_takefocus++;
      }
    }
    XGetInputFocus(d, &f, &r);
    if (f == game2) { ga_landed = 1; }
    else if (ga_landed) { ga_landed = 2; break; }
    usleep(10000);
  }
  ga_stable = (ga_landed == 1);
  XGetInputFocus(d, &f, &r);
  ok("globally-active-self-taken-focus-sticks", ga_stable && extra_takefocus == 0,
      "focus=%lx game2=%lx extra_takefocus=%d", (unsigned long)f, (unsigned long)game2, extra_takefocus);

  XDestroyWindow(d, game2);
  XDestroyWindow(d, game);
  XCloseDisplay(d);
  return 0;
}
