#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <signal.h>

/* Own windows only. Condition polling uses deadlines, never a fixed settling nap.
 * Every stable-state check observes for 200 ms to detect delayed reversals. */
static Display *d;
static Window root, game;
static Atom state, fs, clients;
static int failures;
static double now(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}
static int contains(Window w, Atom prop, unsigned long value) {
    Atom type; int format; unsigned long n, remaining; unsigned char *bytes = NULL;
    int found = 0;
    if (XGetWindowProperty(d,w,prop,0,4096,False,AnyPropertyType,
                          &type,&format,&n,&remaining,&bytes) == Success && format == 32) {
        for (unsigned long i=0;i<n;i++) if (((unsigned long *)bytes)[i]==value) found=1;
    }
    if(bytes) XFree(bytes);
    return found;
}
static void events(void) {
    while(XPending(d)) {
        XEvent e; XNextEvent(d,&e);
        Window w=e.xany.window;
        printf("EVENT mono=%.9f type=%d xid=0x%lx serial=%lu synthetic=%d\n",
               now(),e.type,w,e.xany.serial,e.xany.send_event);
    }
}
static void tick(void) {
    events(); struct pollfd fd={ConnectionNumber(d),POLLIN,0}; poll(&fd,1,10);
}
static void result(const char *name, int passed) {
    Window focus; int revert; XWindowAttributes a;
    XGetInputFocus(d,&focus,&revert); XGetWindowAttributes(d,game,&a);
    printf("RESULT {\"name\":\"%s\",\"passed\":%s,\"xid\":%lu,\"focus\":%lu,"
           "\"fullscreen\":%d,\"managed\":%d,\"mapped\":%d,\"x\":%d,\"y\":%d,\"w\":%d,\"h\":%d}\n",
           name,passed?"true":"false",game,focus,contains(game,state,fs),
           contains(root,clients,game),a.map_state,a.x,a.y,a.width,a.height);
    fflush(stdout); failures+=!passed;
}
static int wait_prop(Window w, Atom prop, unsigned long v, int wanted) {
    double end=now()+5;
    do { if(contains(w,prop,v)==wanted) return 1; tick(); } while(now()<end);
    return 0;
}
static void stable_fs(const char *name,int wanted) {
    int good=1; double end=now()+.2;
    do { if(contains(game,state,fs)!=wanted) good=0; tick(); } while(now()<end);
    result(name,good);
}
static void request(unsigned long action,Atom first,Atom second) {
    XEvent e; memset(&e,0,sizeof(e)); e.xclient.type=ClientMessage;
    e.xclient.window=game; e.xclient.message_type=state; e.xclient.format=32;
    e.xclient.data.l[0]=action; e.xclient.data.l[1]=first; e.xclient.data.l[2]=second;
    e.xclient.data.l[3]=1;
    printf("REQUEST mono=%.9f xid=0x%lx action=%lu first=%lu second=%lu\n",now(),game,action,first,second);
    XSendEvent(d,root,False,SubstructureRedirectMask|SubstructureNotifyMask,&e); XFlush(d);
}
static Window make(Window parent, int override) {
    XSetWindowAttributes a={0}; a.override_redirect=override;
    Window w=XCreateWindow(d,parent,30,40,400,300,0,CopyFromParent,InputOutput,
                          CopyFromParent,CWOverrideRedirect,&a);
    XSelectInput(d,w,StructureNotifyMask|FocusChangeMask|PropertyChangeMask);
    XStoreName(d,w,"forensic-client");
    return w;
}
static void transfer(const char *name,Window to) {
    XMapWindow(d,to); XSync(d,False);
    double end=now()+5; XWindowAttributes a;
    do { XGetWindowAttributes(d,to,&a); if(a.map_state==IsViewable) break; tick(); } while(now()<end);
    XSetInputFocus(d,to,RevertToParent,CurrentTime); XFlush(d);
    stable_fs(name,1);
    XSetInputFocus(d,game,RevertToParent,CurrentTime); XFlush(d);
}
static volatile sig_atomic_t stop;
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
    XWindowAttributes a; Window focus; int revert;
    if(!XGetWindowAttributes(d,w,&a)) return;
    XGetInputFocus(d,&focus,&revert);
    Window transient=None; XGetTransientForHint(d,w,&transient);
    printf("SNAPSHOT mono=%.9f xid=0x%lx pid=%lu transient=0x%lx leader=0x%lx focus=0x%lx managed=%d fs=%d mapped=%d override=%d geom=%d,%d,%d,%d\n",
           now(),w,cardinal(w,"_NET_WM_PID"),transient,cardinal(w,"WM_CLIENT_LEADER"),focus,
           contains(root,clients,w),contains(w,state,fs),a.map_state,a.override_redirect,a.x,a.y,a.width,a.height);
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
    Window r,p,*children=NULL; unsigned count=0;
    if(XQueryTree(d,root,&r,&p,&children,&count)) for(unsigned i=0;i<count;i++)observe_window(children[i],0);
    if(children)XFree(children);
    printf("ATOMS state=%lu fullscreen=%lu clients=%lu active=%lu\n",state,fs,clients,XInternAtom(d,"_NET_ACTIVE_WINDOW",False));
    double end=now()+seconds;
    while(!stop && now()<end) {
        while(XPending(d)) {
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
int main(int argc,char **argv) {
    d=XOpenDisplay(NULL); if(!d) return 2;
    root=DefaultRootWindow(d); state=XInternAtom(d,"_NET_WM_STATE",False);
    fs=XInternAtom(d,"_NET_WM_STATE_FULLSCREEN",False);
    clients=XInternAtom(d,"_NET_CLIENT_LIST",False);
    if(argc>1 && strcmp(argv[1],"--observe")==0) return observe(argc>2?atof(argv[2]):180.0);
    game=make(root,0); XMapWindow(d,game); XFlush(d);
    result("map-managed",wait_prop(root,clients,game,1));
    if(!getenv("AUDIT_PUBLICATION_ONLY")) {
        request(1,fs,None); result("fullscreen-add",wait_prop(game,state,fs,1));
    }
    const char *rounds=getenv("AUDIT_PUBLICATION_ROUNDS");
    if(rounds) {
        Window dialog=make(root,0); XSetTransientForHint(d,dialog,game);
        XMapWindow(d,dialog); XFlush(d); result("stress-dialog-managed",wait_prop(root,clients,dialog,1));
        int good=1;
        for(int i=0;i<atoi(rounds);i++) {
            XUnmapWindow(d,game); XFlush(d);
            if(!wait_prop(root,clients,game,0)) { printf("STALLED round=%d phase=unmap\n",i);good=0;break; }
            XMapWindow(d,game); XFlush(d);
            if(!wait_prop(root,clients,game,1)) { printf("STALLED round=%d phase=remap\n",i);good=0;break; }
        }
        result("publication-converges-without-new-events",good);
        XDestroyWindow(d,dialog); XDestroyWindow(d,game); XCloseDisplay(d);
        puts("COMPLETE fullscreen-lifecycle");return failures?1:0;
    }
    request(1,fs,fs); stable_fs("duplicate-add-two-atoms",1);
    Window child=make(game,0); transfer("child-preserves-fullscreen",child);
    Window popup=make(root,1); transfer("override-preserves-fullscreen",popup);
    int grabbed=XGrabKeyboard(d,popup,False,GrabModeAsync,GrabModeAsync,CurrentTime);
    result("keyboard-grab-acquired",grabbed==GrabSuccess);
    stable_fs("grab-preserves-fullscreen",1); XUngrabKeyboard(d,CurrentTime); XFlush(d);
    stable_fs("ungrab-preserves-fullscreen",1);
    Window dialog=make(root,0); XSetTransientForHint(d,dialog,game);
    transfer("transient-preserves-fullscreen",dialog);
    Window *tree=NULL,parent,returned; unsigned count=0;
    int query=XQueryTree(d,root,&returned,&parent,&tree,&count),gi=-1,di=-1;
    for(unsigned i=0;i<count;i++){if(tree[i]==game)gi=(int)i;if(tree[i]==dialog)di=(int)i;}
    result("transient-above-fullscreen",query && gi>=0 && di>gi); if(tree)XFree(tree);
    Window unrelated=make(root,0); XMapWindow(d,unrelated); XFlush(d);
    result("unrelated-managed",wait_prop(root,clients,unrelated,1));
    transfer("unrelated-focus-preserves-fullscreen",unrelated);
    XSetInputFocus(d,root,RevertToParent,CurrentTime); XFlush(d);
    stable_fs("root-focus-preserves-fullscreen",1);
    XSetInputFocus(d,game,RevertToParent,CurrentTime); XFlush(d);
    XDestroyWindow(d,unrelated); XFlush(d);
    XResizeWindow(d,game,123,97); XFlush(d); stable_fs("configure-preserves-fullscreen",1);
    XWindowAttributes fullscreen_geometry; XGetWindowAttributes(d,game,&fullscreen_geometry);
    result("configure-keeps-screen-geometry",fullscreen_geometry.x==0 && fullscreen_geometry.y==0 &&
           fullscreen_geometry.width==DisplayWidth(d,DefaultScreen(d)) && fullscreen_geometry.height==DisplayHeight(d,DefaultScreen(d)));
    request(2,fs,None); result("toggle-leaves",wait_prop(game,state,fs,0));
    request(2,None,fs); result("second-atom-toggle-enters",wait_prop(game,state,fs,1));
    request(0,fs,None); result("remove-leaves",wait_prop(game,state,fs,0));
    request(0,fs,None); stable_fs("duplicate-remove",0);
    request(1,fs,None); result("reenter-before-unmap",wait_prop(game,state,fs,1));
    XSetInputFocus(d,dialog,RevertToParent,CurrentTime); XFlush(d);
    stable_fs("dialog-focused-before-withdraw",1);
    XUnmapWindow(d,game); XFlush(d);
    result("unmap-unmanages",wait_prop(root,clients,game,0));
    XWindowAttributes a; result("unmap-resource-exists",XGetWindowAttributes(d,game,&a) && a.map_state==IsUnmapped);
    XMapWindow(d,game); XFlush(d); result("remap-managed",wait_prop(root,clients,game,1));
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
