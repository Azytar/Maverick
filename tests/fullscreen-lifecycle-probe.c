#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <errno.h>
#include <limits.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* One connection owns all synthetic windows. Property replies are bounded;
 * reading another client's properties never injects a wakeup into the WM. */
static Display *d;
static Window root, game;
static Atom state, fs, clients;
static int failures;
static unsigned completed;
static volatile sig_atomic_t stop;

enum Read { READ_ERROR, READ_ABSENT, READ_VALUE };
static const char *const lifecycle_checks[] = {
    "map-managed", "fullscreen-add", "duplicate-add-two-atoms",
    "child-preserves-fullscreen", "override-preserves-fullscreen",
    "keyboard-grab-acquired", "grab-preserves-fullscreen", "ungrab-preserves-fullscreen",
    "transient-preserves-fullscreen", "transient-above-fullscreen", "unrelated-managed",
    "unrelated-focus-preserves-fullscreen", "root-focus-preserves-fullscreen",
    "configure-preserves-fullscreen", "configure-keeps-screen-geometry", "toggle-leaves",
    "second-atom-toggle-enters", "remove-leaves", "duplicate-remove", "reenter-before-unmap",
    "dialog-focused-before-withdraw", "unmap-unmanages", "unmap-resource-exists",
    "remap-managed", "remap-explicit-fullscreen", "invalid-action-must-not-toggle", "destroy-unmanages"
};
static const char *const property_checks[] = {
    "property-absent", "property-empty", "property-present", "property-wrong-type",
    "property-wrong-format", "property-truncated", "property-required-missing"
};

static void die(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    fputs("ERROR ", stderr); vfprintf(stderr, fmt, args); fputc('\n', stderr);
    va_end(args);
    exit(2);
}
static double now(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t) != 0) die("clock_gettime: %s", strerror(errno));
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}
static long number(const char *text, long max, const char *label) {
    char *end;
    errno = 0;
    long value = strtol(text, &end, 10);
    if (!*text || text[0] < '0' || text[0] > '9' || *end || errno || value < 1 || value > max)
        die("%s must be an integer in 1..%ld", label, max);
    return value;
}
/* Xlib represents format-32 data as unsigned long, including on LP64 hosts. */
static enum Read decode(Atom actual, int format, unsigned long n, unsigned long left,
                        const unsigned char *data, Atom expected, bool required) {
    if (actual == None)
        return !required && format == 0 && n == 0 && left == 0 ? READ_ABSENT : READ_ERROR;
    if (actual != expected || format != 32 || left || n > 4096 || (n && !data))
        return READ_ERROR;
    return READ_VALUE;
}
static enum Read read_words(Window w, Atom prop, Atom expected, bool required,
                            unsigned long *words, unsigned long *count) {
    Atom actual = None;
    int format = 0;
    unsigned long n = 0, left = 0;
    unsigned char *bytes = NULL;
    int status = XGetWindowProperty(d, w, prop, 0, 4096, False, AnyPropertyType,
                                   &actual, &format, &n, &left, &bytes);
    enum Read read = status == Success ? decode(actual, format, n, left, bytes, expected, required) : READ_ERROR;
    *count = 0;
    if (read == READ_VALUE) {
        if (n) memcpy(words, bytes, n * sizeof(*words));
        *count = n;
    }
    if (bytes) XFree(bytes);
    return read;
}
static bool contains(Window w, Atom prop, unsigned long value) {
    unsigned long words[4096], n;
    bool membership = prop == clients;
    enum Read read = read_words(w, prop, membership ? XA_WINDOW : XA_ATOM, membership, words, &n);
    if (read == READ_ERROR) die("invalid property xid=0x%lx atom=%lu", w, prop);
    for (unsigned long i = 0; i < n; i++) if (words[i] == value) return true;
    return false;
}
static XWindowAttributes attributes(Window w) {
    XWindowAttributes a = {0};
    if (!XGetWindowAttributes(d, w, &a)) die("cannot read attributes xid=0x%lx", w);
    return a;
}
static Window input_focus(void) {
    Window focus = None; int revert;
    if (!XGetInputFocus(d, &focus, &revert)) die("cannot read input focus");
    return focus;
}
static int x_error(Display *display, XErrorEvent *e) {
    (void)display;
    die("X11 code=%u request=%u minor=%u xid=0x%lx serial=%lu",
        e->error_code, e->request_code, e->minor_code, e->resourceid, e->serial);
    return 0;
}
static void events(void) {
    /* Bound work so event floods cannot suppress deadlines or SIGTERM. */
    for (unsigned i = 0; i < 256 && XPending(d); i++) {
        XEvent e; XNextEvent(d, &e);
        Window w = e.xany.window;
        if (e.type == MapNotify) w = e.xmap.window;
        else if (e.type == UnmapNotify) w = e.xunmap.window;
        else if (e.type == ConfigureNotify) w = e.xconfigure.window;
        else if (e.type == DestroyNotify) w = e.xdestroywindow.window;
        printf("EVENT mono=%.9f type=%d xid=0x%lx serial=%lu synthetic=%d\n",
               now(), e.type, w, e.xany.serial, e.xany.send_event);
    }
}
static void tick(void) {
    events();
    struct pollfd fd = {ConnectionNumber(d), POLLIN, 0};
    if (poll(&fd, 1, 10) < 0 && errno != EINTR) die("poll: %s", strerror(errno));
    if (fd.revents & (POLLERR | POLLHUP | POLLNVAL)) die("X11 connection closed");
}
static void result(const char *name, bool passed) {
    XWindowAttributes a = attributes(game);
    Window focus = input_focus();
    bool fullscreen = contains(game, state, fs), managed = contains(root, clients, game);
    printf("RESULT {\"name\":\"%s\",\"passed\":%s,\"xid\":%lu,\"focus\":%lu,"
           "\"fullscreen\":%s,\"managed\":%s,\"mapped\":%d,\"x\":%d,\"y\":%d,\"w\":%d,\"h\":%d,\"completed\":%u}\n",
           name, passed ? "true" : "false", game, focus, fullscreen ? "true" : "false",
           managed ? "true" : "false", a.map_state, a.x, a.y, a.width, a.height, completed);
    fflush(stdout); failures += !passed;
    /* Later operations may require the condition just checked (e.g. viewability
     * before XSetInputFocus). A failed prerequisite must not cascade into BadMatch. */
    if (!passed) exit(1);
}
static bool wait_prop(Window w, Atom prop, unsigned long v, bool wanted) {
    double end = now() + 5;
    do {
        unsigned long words[4096], n;
        enum Read read=read_words(w,prop,prop==clients ? XA_WINDOW : XA_ATOM,false,words,&n);
        if(read==READ_ERROR)die("invalid property while waiting xid=0x%lx atom=%lu",w,prop);
        /* A missing client list is not evidence that a withdrawn client was
         * removed. Wait for the WM to publish a typed list, even if empty. */
        if(read!=READ_ABSENT || prop!=clients) {
            bool found=false;
            for(unsigned long i=0;i<n;i++)if(words[i]==v)found=true;
            if(found==wanted)return true;
        }
        tick();
    } while (now() < end);
    return false;
}
static bool wait_map(Window w, int wanted) {
    double end = now() + 5;
    do { if (attributes(w).map_state == wanted) return true; tick(); } while (now() < end);
    return false;
}
static bool wait_focus(Window wanted) {
    double end = now() + 5;
    do { if (input_focus() == wanted) return true; tick(); } while (now() < end);
    return false;
}
static void stable_fs(const char *name, bool wanted) {
    bool good = true; double end = now() + .2;
    do { if (contains(game, state, fs) != wanted) good = false; tick(); } while (now() < end);
    result(name, good);
}
static void focus_and_hold(const char *name, Window to, Window expected) {
    XSetInputFocus(d, to, RevertToParent, CurrentTime); XFlush(d);
    bool good = wait_focus(expected); double end = now() + .2;
    do {
        if (input_focus() != expected || !contains(game, state, fs)) good = false;
        tick();
    } while (now() < end);
    result(name, good);
}
static void request(unsigned long action, Atom first, Atom second) {
    XEvent e = {0}; e.xclient.type = ClientMessage;
    e.xclient.window = game; e.xclient.message_type = state; e.xclient.format = 32;
    e.xclient.data.l[0] = (long)action; e.xclient.data.l[1] = (long)first; e.xclient.data.l[2] = (long)second;
    e.xclient.data.l[3] = 1;
    printf("REQUEST mono=%.9f xid=0x%lx action=%lu first=%lu second=%lu\n", now(), game, action, first, second);
    if (!XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &e)) die("XSendEvent failed");
    XFlush(d);
}
static Window make(Window parent, bool override) {
    XSetWindowAttributes a = {0}; a.override_redirect = override;
    Window w = XCreateWindow(d, parent, 30, 40, 400, 300, 0, CopyFromParent, InputOutput,
                            CopyFromParent, CWOverrideRedirect, &a);
    XSelectInput(d, w, StructureNotifyMask | FocusChangeMask | PropertyChangeMask);
    XStoreName(d, w, "forensic-client");
    return w;
}
static void transfer(const char *name, Window to, Window expected) {
    XMapWindow(d, to); XFlush(d);
    if (!wait_map(to, IsViewable)) die("focus target did not become viewable xid=0x%lx", to);
    focus_and_hold(name, to, expected);
    XSetInputFocus(d, game, RevertToParent, CurrentTime); XFlush(d);
    if (!wait_focus(game)) die("focus did not return to main window");
}
static void describe(const char *which, bool only) {
    const char *const *names = lifecycle_checks;
    size_t n = sizeof(lifecycle_checks) / sizeof(*lifecycle_checks);
    if (!strcmp(which, "properties")) { names = property_checks; n = sizeof(property_checks) / sizeof(*property_checks); }
    if (!strcmp(which, "publication")) {
        printf("[\"map-managed\",%s\"stress-dialog-managed\",\"publication-converges-without-new-events\"]\n",
               only ? "" : "\"fullscreen-add\",");
        return;
    }
    putchar('[');
    for (size_t i = 0; i < n; i++) printf("%s\"%s\"", i ? "," : "", names[i]);
    puts("]");
}
static void self_test(void) {
    unsigned long word = 42;
    const unsigned char *p = (const unsigned char *)&word;
    if (decode(None, 0, 0, 0, NULL, XA_ATOM, false) != READ_ABSENT ||
        decode(None, 0, 0, 0, NULL, XA_WINDOW, true) != READ_ERROR ||
        decode(XA_ATOM, 32, 0, 0, NULL, XA_ATOM, true) != READ_VALUE ||
        decode(XA_ATOM, 32, 1, 0, p, XA_ATOM, true) != READ_VALUE ||
        decode(XA_CARDINAL, 32, 1, 0, p, XA_ATOM, false) != READ_ERROR ||
        decode(XA_ATOM, 8, 1, 0, p, XA_ATOM, false) != READ_ERROR ||
        decode(XA_ATOM, 32, 1, 4, p, XA_ATOM, false) != READ_ERROR ||
        decode(XA_ATOM, 32, 4097, 0, p, XA_ATOM, false) != READ_ERROR ||
        decode(XA_ATOM, 32, 1, 0, NULL, XA_ATOM, false) != READ_ERROR)
        die("property decoder self-test failed");
    puts("SELFTEST property-decoder passed");
}
static void check(const char *name, bool passed) {
    printf("RESULT {\"name\":\"%s\",\"passed\":%s}\n",name,passed ? "true" : "false");
    fflush(stdout);
    if(!passed)exit(1);
}
static int properties(void) {
    game = make(root, true);
    Atom prop = XInternAtom(d, "_MAVERICK_PROBE_PROPERTY", False);
    unsigned long words[4097] = {0}, count;
    check("property-absent", read_words(game, prop, XA_ATOM, false, words, &count) == READ_ABSENT);
    XChangeProperty(d, game, prop, XA_ATOM, 32, PropModeReplace, NULL, 0);
    check("property-empty", read_words(game, prop, XA_ATOM, true, words, &count) == READ_VALUE && count == 0);
    words[0] = fs;
    XChangeProperty(d, game, prop, XA_ATOM, 32, PropModeReplace, (const unsigned char *)words, 1);
    check("property-present", read_words(game, prop, XA_ATOM, true, words, &count) == READ_VALUE && count == 1 && words[0] == fs);
    XChangeProperty(d, game, prop, XA_CARDINAL, 32, PropModeReplace, (const unsigned char *)words, 1);
    check("property-wrong-type", read_words(game, prop, XA_ATOM, false, words, &count) == READ_ERROR);
    XChangeProperty(d, game, prop, XA_ATOM, 8, PropModeReplace, (const unsigned char *)"x", 1);
    check("property-wrong-format", read_words(game, prop, XA_ATOM, false, words, &count) == READ_ERROR);
    XChangeProperty(d, game, prop, XA_ATOM, 32, PropModeReplace, (const unsigned char *)words, 4097);
    check("property-truncated", read_words(game, prop, XA_ATOM, false, words, &count) == READ_ERROR);
    XDeleteProperty(d, game, prop);
    check("property-required-missing", read_words(game, prop, XA_WINDOW, true, words, &count) == READ_ERROR);
    XDestroyWindow(d, game); XCloseDisplay(d);
    puts("COMPLETE fullscreen-lifecycle"); return 0;
}
static int observer_has(Window w, Atom prop, unsigned long value) {
    unsigned long words[4096], n;
    enum Read read=read_words(w, prop, prop == clients ? XA_WINDOW : XA_ATOM, false, words, &n);
    if(read==READ_ERROR)return -1;
    if(read==READ_ABSENT)return 0;
    for (unsigned long i=0;i<n;i++) if(words[i]==value)return true;
    return false;
}
static void stopped(int signal_number) { (void)signal_number; stop=1; }
static int observer_error(Display *display, XErrorEvent *e) {
    (void)display;
    printf("XERROR mono=%.9f xid=0x%lx code=%u request=%u\n", now(),e->resourceid,e->error_code,e->request_code);
    return 0;
}
static unsigned long cardinal(Window w,const char *name) {
    Atom actual; int format; unsigned long n,left; unsigned char *p=NULL; unsigned long v=0;
    if(XGetWindowProperty(d,w,XInternAtom(d,name,False),0,1,False,AnyPropertyType,
                         &actual,&format,&n,&left,&p)==Success && format==32 && n) v=*(unsigned long *)p;
    if(p)XFree(p);
    return v;
}
static void observe_window(Window w,int depth) {
    XWindowAttributes a; Window focus=None; int revert;
    if(!XGetWindowAttributes(d,w,&a)) return;
    if(!XGetInputFocus(d,&focus,&revert))return;
    Window transient=None; XGetTransientForHint(d,w,&transient);
    printf("SNAPSHOT mono=%.9f xid=0x%lx pid=%lu transient=0x%lx leader=0x%lx focus=0x%lx managed=%d fs=%d mapped=%d override=%d geom=%d,%d,%d,%d\n",
           now(),w,cardinal(w,"_NET_WM_PID"),transient,cardinal(w,"WM_CLIENT_LEADER"),focus,
           observer_has(root,clients,w),observer_has(w,state,fs),a.map_state,a.override_redirect,a.x,a.y,a.width,a.height);
    XSelectInput(d,w,StructureNotifyMask|SubstructureNotifyMask|FocusChangeMask|PropertyChangeMask);
    if(depth<4) {
        Window r,p,*children=NULL; unsigned count=0;
        if(XQueryTree(d,w,&r,&p,&children,&count)) for(unsigned i=0;i<count;i++)observe_window(children[i],depth+1);
        if(children)XFree(children);
    }
}
static int observe(double seconds) {
    XSetErrorHandler(observer_error); signal(SIGINT,stopped); signal(SIGTERM,stopped);
    XSelectInput(d,root,SubstructureNotifyMask|PropertyChangeMask|FocusChangeMask);
    XSync(d,False); puts("READY observer"); fflush(stdout);
    Window r,p,*children=NULL; unsigned count=0;
    if(XQueryTree(d,root,&r,&p,&children,&count)) for(unsigned i=0;i<count;i++)observe_window(children[i],0);
    if(children)XFree(children);
    printf("ATOMS state=%lu fullscreen=%lu clients=%lu active=%lu\n",state,fs,clients,XInternAtom(d,"_NET_ACTIVE_WINDOW",False));
    double end=now()+seconds;
    while(!stop && now()<end) {
        for(unsigned budget=0;budget<256 && !stop && now()<end && XPending(d);budget++) {
            XEvent e; XNextEvent(d,&e);
            printf("OBSERVE mono=%.9f type=%d xid=0x%lx serial=%lu synthetic=%d\n",now(),e.type,e.xany.window,e.xany.serial,e.xany.send_event);
            Window w=e.xany.window;
            if(e.type==ClientMessage) {
                printf("CLIENT_MESSAGE mono=%.9f xid=0x%lx type=%lu format=%d data=%ld,%ld,%ld,%ld,%ld\n",now(),e.xclient.window,e.xclient.message_type,e.xclient.format,
                       e.xclient.data.l[0],e.xclient.data.l[1],e.xclient.data.l[2],e.xclient.data.l[3],e.xclient.data.l[4]);
                w=e.xclient.window;
            } else if(e.type==CreateNotify) w=e.xcreatewindow.window;
            else if(e.type==MapNotify) w=e.xmap.window;
            else if(e.type==UnmapNotify) w=e.xunmap.window;
            else if(e.type==DestroyNotify) {
                printf("DESTROY mono=%.9f xid=0x%lx\n",now(),e.xdestroywindow.window); continue;
            } else if(e.type==ConfigureNotify) w=e.xconfigure.window;
            if(w!=root)observe_window(w,4);
            else { Window focus; int revert; XGetInputFocus(d,&focus,&revert);
                printf("ROOT mono=%.9f focus=0x%lx active=0x%lx\n",now(),focus,cardinal(root,"_NET_ACTIVE_WINDOW")); }
        }
        fflush(stdout); struct pollfd fd={ConnectionNumber(d),POLLIN,0}; poll(&fd,1,100);
    }
    XCloseDisplay(d); return 0;
}
int main(int argc, char **argv) {
    const char *raw = getenv("AUDIT_PUBLICATION_ROUNDS");
    long rounds = raw ? number(raw,4096,"AUDIT_PUBLICATION_ROUNDS") : 128;
    const char *which = raw ? "publication" : "lifecycle";
    const char *flag = getenv("AUDIT_PUBLICATION_ONLY");
    if(flag && strcmp(flag,"0") && strcmp(flag,"1")) die("AUDIT_PUBLICATION_ONLY must be 0 or 1");
    bool only=flag && !strcmp(flag,"1"), describe_only=false, self=false, observing=false;
    double seconds=180;
    for(int i=1;i<argc;i++) {
        if(!strcmp(argv[i],"--describe")) describe_only=true;
        else if(!strcmp(argv[i],"--self-test")) self=true;
        else if(!strcmp(argv[i],"--case") && i+1<argc) which=argv[++i];
        else if(!strcmp(argv[i],"--rounds") && i+1<argc) { rounds=number(argv[++i],4096,"rounds"); which="publication"; }
        else if(!strcmp(argv[i],"--observe")) {
            observing=true;
            if(i+1<argc) {
                char *end;errno=0;const char *text=argv[++i]; seconds=strtod(text,&end);
                if(!*text || *end || errno || !isfinite(seconds) || seconds<=0 || seconds>3600)die("observer duration must be in (0,3600]");
            }
        } else die("unknown or incomplete option: %s",argv[i]);
    }
    if(strcmp(which,"lifecycle") && strcmp(which,"publication") && strcmp(which,"properties"))die("unknown case: %s",which);
    if(only && strcmp(which,"publication"))die("publication-only requires publication case");
    if(describe_only) { describe(which,only);return 0; }
    if(self) { self_test();return 0; }
    d=XOpenDisplay(NULL);if(!d)die("cannot open private DISPLAY");
    XSetErrorHandler(x_error);
    root=DefaultRootWindow(d); state=XInternAtom(d,"_NET_WM_STATE",False);
    fs=XInternAtom(d,"_NET_WM_STATE_FULLSCREEN",False); clients=XInternAtom(d,"_NET_CLIENT_LIST",False);
    if(observing)return observe(seconds);
    if(!strcmp(which,"properties"))return properties();
    game=make(root,0); XMapWindow(d,game); XFlush(d);
    result("map-managed",wait_prop(root,clients,game,1) && wait_map(game,IsViewable));
    if(!only) {
        request(1,fs,None); result("fullscreen-add",wait_prop(game,state,fs,1));
    }
    if(!strcmp(which,"publication")) {
        Window dialog=make(root,0); XSetTransientForHint(d,dialog,game);
        XMapWindow(d,dialog); XFlush(d); result("stress-dialog-managed",wait_prop(root,clients,dialog,1));
        int good=1;
        for(long i=0;i<rounds;i++) {
            XUnmapWindow(d,game); XFlush(d);
            if(!wait_prop(root,clients,game,0)) { printf("STALLED round=%ld phase=unmap\n",i);good=0;break; }
            XMapWindow(d,game); XFlush(d);
            if(!wait_prop(root,clients,game,1) || !wait_map(game,IsViewable)) { printf("STALLED round=%ld phase=remap\n",i);good=0;break; }
            completed++;
        }
        result("publication-converges-without-new-events",good && completed==(unsigned)rounds);
        XDestroyWindow(d,dialog); XDestroyWindow(d,game); XCloseDisplay(d);
        puts("COMPLETE fullscreen-lifecycle");return failures?1:0;
    }
    request(1,fs,fs); stable_fs("duplicate-add-two-atoms",1);
    Window child=make(game,0); transfer("child-preserves-fullscreen",child,child);
    Window popup=make(root,1); transfer("override-preserves-fullscreen",popup,popup);
    int grabbed=XGrabKeyboard(d,popup,False,GrabModeAsync,GrabModeAsync,CurrentTime);
    result("keyboard-grab-acquired",grabbed==GrabSuccess);
    stable_fs("grab-preserves-fullscreen",1); XUngrabKeyboard(d,CurrentTime); XFlush(d);
    stable_fs("ungrab-preserves-fullscreen",1);
    Window dialog=make(root,0); XSetTransientForHint(d,dialog,game);
    transfer("transient-preserves-fullscreen",dialog,dialog);
    Window *tree=NULL,parent,returned; unsigned count=0;
    int query=XQueryTree(d,root,&returned,&parent,&tree,&count),gi=-1,di=-1;
    if(!query)die("XQueryTree failed");
    for(unsigned i=0;i<count;i++){if(tree[i]==game)gi=(int)i;if(tree[i]==dialog)di=(int)i;}
    result("transient-above-fullscreen",query && gi>=0 && di>gi); if(tree)XFree(tree);
    Window unrelated=make(root,0); XMapWindow(d,unrelated); XFlush(d);
    result("unrelated-managed",wait_prop(root,clients,unrelated,1));
    transfer("unrelated-focus-preserves-fullscreen",unrelated,game);
    focus_and_hold("root-focus-preserves-fullscreen",root,game);
    XSetInputFocus(d,game,RevertToParent,CurrentTime); XFlush(d);
    XDestroyWindow(d,unrelated); XFlush(d);
    XResizeWindow(d,game,123,97); XFlush(d); stable_fs("configure-preserves-fullscreen",1);
    XWindowAttributes fullscreen_geometry; fullscreen_geometry=attributes(game);
    result("configure-keeps-screen-geometry",fullscreen_geometry.x==0 && fullscreen_geometry.y==0 &&
           fullscreen_geometry.width==DisplayWidth(d,DefaultScreen(d)) && fullscreen_geometry.height==DisplayHeight(d,DefaultScreen(d)));
    request(2,fs,None); result("toggle-leaves",wait_prop(game,state,fs,0));
    request(2,None,fs); result("second-atom-toggle-enters",wait_prop(game,state,fs,1));
    request(0,fs,None); result("remove-leaves",wait_prop(game,state,fs,0));
    request(0,fs,None); stable_fs("duplicate-remove",0);
    request(1,fs,None); result("reenter-before-unmap",wait_prop(game,state,fs,1));
    focus_and_hold("dialog-focused-before-withdraw",dialog,dialog);
    XUnmapWindow(d,game); XFlush(d);
    result("unmap-unmanages",wait_prop(root,clients,game,0));
    XWindowAttributes a; result("unmap-resource-exists",XGetWindowAttributes(d,game,&a) && a.map_state==IsUnmapped);
    XMapWindow(d,game); XFlush(d); result("remap-managed",wait_prop(root,clients,game,1) && wait_map(game,IsViewable));
    /* No expectation that withdrawal preserves a user fullscreen toggle. */
    XDestroyWindow(d,dialog); XDestroyWindow(d,popup); XFlush(d);
    request(1,fs,None); result("remap-explicit-fullscreen",wait_prop(game,state,fs,1));
    request(99,fs,None); stable_fs("invalid-action-must-not-toggle",1);
    XDestroyWindow(d,game); XFlush(d);
    int removed=wait_prop(root,clients,game,0);
    printf("RESULT {\"name\":\"destroy-unmanages\",\"passed\":%s}\n",removed?"true":"false");
    failures+=!removed;
    XCloseDisplay(d); puts("COMPLETE fullscreen-lifecycle"); return failures?1:0;
}
