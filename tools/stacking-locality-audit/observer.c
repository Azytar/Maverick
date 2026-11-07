#define _GNU_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/resource.h>
#include <unistd.h>
#include <sys/socket.h>

/* Only this connection's notification masks are changed. No redirect, window,
 * focus, stacking, property write, atom creation or content query is needed. */
static Display *d;
static Window root;
static unsigned errors, requests;
static uint64_t began, last_notice;
static unsigned long event_counts[7];
static int dirty = 1, notified;
static const char *names[] = {
    "_NET_CLIENT_LIST", "_NET_CLIENT_LIST_STACKING", "_NET_WM_STATE",
    "_NET_WM_WINDOW_TYPE", "WM_TRANSIENT_FOR", "_NET_WM_STATE_FULLSCREEN",
    "_NET_WM_STATE_MAXIMIZED_VERT", "_NET_WM_STATE_MAXIMIZED_HORZ",
    "_NET_WM_STATE_STICKY", "_NET_WM_WINDOW_TYPE_DOCK", "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_WINDOW_TYPE_MENU", "_NET_WM_WINDOW_TYPE_POPUP_MENU",
    "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU", "_NET_WM_WINDOW_TYPE_TOOLTIP",
    "_NET_WM_WINDOW_TYPE_NOTIFICATION", "_NET_ACTIVE_WINDOW", "_NET_CURRENT_DESKTOP"
};
static Atom atoms[18];
typedef struct {
    Window id;
    XWindowAttributes attr;
    int attr_valid, meta_valid, meta_complete, subscribed;
    unsigned flags;
    Window transient;
} Node;
static Node *nodes;
static size_t nodes_n;
typedef struct { int kind; Window id; uint64_t ns; } Note;
static Note notes[4096];
static unsigned notes_n, overflow;

static uint64_t clock_ns(clockid_t clock) {
    struct timespec t;
    if (clock_gettime(clock, &t)) exit(2);
    return (uint64_t)t.tv_sec * 1000000000 + (uint64_t)t.tv_nsec;
}
static uint64_t ns(void) {return clock_ns(CLOCK_MONOTONIC);}
static int on_error(Display *display, XErrorEvent *e) {
    (void)display; (void)e; ++errors; return 0;
}
static Node *node(Window w) {
    for (size_t i=0;i<nodes_n;i++) if(nodes[i].id==w) return &nodes[i];
    if(nodes_n>=65536) { fputs("too many root nodes\n",stderr); exit(2); }
    Node *p=realloc(nodes,(nodes_n+1)*sizeof(*nodes));
    if(!p) exit(2);
    nodes=p;
    nodes[nodes_n]=(Node){.id=w};
    return &nodes[nodes_n++];
}
static void intern_existing(void) {
    Atom next[18];
    ++requests;
    if(!XInternAtoms(d,(char**)names,18,True,next)) exit(2);
    if(memcmp(atoms,next,sizeof(atoms))) {
        memcpy(atoms,next,sizeof(atoms));
        for(size_t i=0;i<nodes_n;i++) nodes[i].meta_valid=0;
    }
}
/* Missing is valid absence for optional per-window hints, but not proof of an
 * empty EWMH client list. Truncation and wrong types are never valid emptiness. */
static int property(Window w, Atom key, Atom wanted, unsigned long **out,
                    unsigned long *n, int *present) {
    Atom type=None; int format=0; unsigned long after=0;
    unsigned char *data=NULL;
    *out=NULL; *n=0; *present=0;
    if(key==None) return 1;
    ++requests;
    int ok=XGetWindowProperty(d,w,key,0,65536,False,wanted,&type,&format,n,&after,&data)==Success;
    *present=type!=None;
    if(!ok || after || (type!=None && (type!=wanted || format!=32))) {
        if(data) XFree(data);
        *n=0; return 0;
    }
    *out=(unsigned long*)data;
    return 1;
}
static int has(const unsigned long *data,unsigned long n,Atom value) {
    if(value==None) return 0;
    for(unsigned long i=0;i<n;i++) if(data[i]==value) return 1;
    return 0;
}
static void metadata(Node *p) {
    unsigned long *data=NULL,n=0; int present=0;
    p->flags=0; p->transient=None; p->meta_complete=1;
    int ok=property(p->id,atoms[2],XA_ATOM,&data,&n,&present);
    p->meta_complete&=ok;
    for(int i=5;i<=8;i++) if(ok && has(data,n,atoms[i])) p->flags|=1u<<(unsigned)(i-5);
    if(data) XFree(data);
    data=NULL;
    ok=property(p->id,atoms[3],XA_ATOM,&data,&n,&present);
    p->meta_complete&=ok;
    for(int i=9;i<16;i++) if(ok && has(data,n,atoms[i])) p->flags|=1u<<(unsigned)(i-5);
    if(data) XFree(data);
    data=NULL;
    ok=property(p->id,atoms[4],XA_WINDOW,&data,&n,&present);
    if(present && n!=1) ok=0;
    if(ok && n==1) p->transient=data[0];
    p->meta_complete&=ok;
    if(data) XFree(data);
    p->meta_valid=1;
}
static void events(void) {
    while(XPending(d)) {
        XEvent e; XNextEvent(d,&e);
        int kind=-1; Window w=None;
        switch(e.type) {
        case MapNotify: kind=0; w=e.xmap.window; break;
        case UnmapNotify: kind=1; w=e.xunmap.window; break;
        case ConfigureNotify: kind=2; w=e.xconfigure.window; break;
        case ReparentNotify: kind=3; w=e.xreparent.window; break;
        case DestroyNotify: kind=4; w=e.xdestroywindow.window; break;
        case PropertyNotify:
            w=e.xproperty.window;
            if(w==root) {
                if(e.xproperty.atom!=atoms[0] && e.xproperty.atom!=atoms[1] && e.xproperty.atom!=atoms[16] && e.xproperty.atom!=atoms[17]) break;
            } else if(e.xproperty.atom!=atoms[2] && e.xproperty.atom!=atoms[3] && e.xproperty.atom!=atoms[4]) break;
            kind=5; break;
        case CreateNotify: kind=6; w=e.xcreatewindow.window; break;
        default: break;
        }
        if(kind<0) continue;
        dirty=1; event_counts[kind]++;
        if(notes_n<4096) notes[notes_n++]=(Note){kind,w,ns()-began}; else overflow++;
        if(w!=root) {
            Node *p=node(w);
            if(kind==5) p->meta_valid=0;
            else p->attr_valid=0;
            if(kind==4) {p->meta_valid=0;p->subscribed=0;}
        }
    }
}
static void array(const unsigned long *data,unsigned long n) {
    putchar('[');
    for(unsigned long i=0;i<n;i++) printf("%s%lu",i?",":"",data[i]);
    putchar(']');
}
static void capture(int refresh) {
    uint64_t start=ns(); unsigned error0=errors,request0=requests;
    if(refresh) {
        intern_existing();
        for(size_t i=0;i<nodes_n;i++) nodes[i].attr_valid=nodes[i].meta_valid=0;
    }
    events();
    unsigned events_before_queries=notes_n;
    Window rr,parent,*children=NULL,*again=NULL;unsigned n=0,m=0;
    ++requests;
    if(!XQueryTree(d,root,&rr,&parent,&children,&n) || n>65536) exit(2);
    for(unsigned i=0;i<n;i++) {
        Node *p=node(children[i]);
        if(!p->attr_valid) {
            ++requests;
            p->attr_valid=XGetWindowAttributes(d,p->id,&p->attr)!=0;
            if(p->attr_valid && !p->subscribed) {++requests;XSelectInput(d,p->id,PropertyChangeMask);p->subscribed=1;}
        }
        if(!p->meta_valid && p->attr_valid) metadata(p);
    }
    unsigned long *managed=NULL,*stacking=NULL,nmanaged=0,nstacking=0;int present_m=0,present_s=0;
    int valid_m=property(root,atoms[0],XA_WINDOW,&managed,&nmanaged,&present_m) && present_m;
    int valid_s=property(root,atoms[1],XA_WINDOW,&stacking,&nstacking,&present_s) && present_s;
    ++requests;
    int stable=XQueryTree(d,root,&rr,&parent,&again,&m) && n==m && (!n || !memcmp(children,again,n*sizeof(Window)));
    /* Changes during the queries are retained; metadata is invalidated for the
     * next capture. Stability is an observed bracket, never an atomic snapshot. */
    events();
    int followup=notes_n>events_before_queries;
#ifdef AUDIT_REVERSE_ROOT
    for(unsigned i=0;i<n/2;i++) {Window t=children[i];children[i]=children[n-1-i];children[n-1-i]=t;}
#endif
    struct rusage usage; if(getrusage(RUSAGE_SELF,&usage)) exit(2);
    printf("{\"observer_cpu_ns\":%llu,\"observer_peak_rss_kib\":%ld,\"snapshot\":true,\"t_start_ns\":%llu,\"t_end_ns\":%llu,\"root_stable\":%s,\"x_errors\":%u,\"xlib_query_and_subscription_calls\":%u,\"overflow\":%u,\"managed_valid\":%s,\"stacking_valid\":%s,\"managed\":",(unsigned long long)clock_ns(CLOCK_PROCESS_CPUTIME_ID),usage.ru_maxrss,(unsigned long long)(start-began),(unsigned long long)(ns()-began),stable?"true":"false",errors-error0,requests-request0,overflow,valid_m?"true":"false",valid_s?"true":"false");
    array(managed,nmanaged); fputs(",\"published_stack\":",stdout);array(stacking,nstacking);
    fputs(",\"windows\":[",stdout);
    for(unsigned i=0;i<n;i++) {
        Node *p=node(children[i]);
        printf("%s{\"id\":%lu,\"attributes_valid\":%s,\"metadata_complete\":%s",i?",":"",p->id,p->attr_valid?"true":"false",(p->meta_complete && p->meta_valid)?"true":"false");
        if(p->attr_valid) printf(",\"map_state\":%d,\"input_only\":%s,\"override_redirect\":%s,\"geom\":[%d,%d,%d,%d,%d]",p->attr.map_state,p->attr.class==InputOnly?"true":"false",p->attr.override_redirect?"true":"false",p->attr.x,p->attr.y,p->attr.width,p->attr.height,p->attr.border_width);
        printf(",\"flags\":%u,\"transient\":%lu}",p->flags,p->transient);
    }
    fputs("],\"events\":[",stdout);
    for(unsigned i=0;i<notes_n;i++) printf("%s{\"kind\":%d,\"window\":%lu,\"t_ns\":%llu}",i?",":"",notes[i].kind,notes[i].id,(unsigned long long)notes[i].ns);
    fputs("],\"event_counts\":[",stdout);
    for(unsigned i=0;i<7;i++) printf("%s%lu",i?",":"",event_counts[i]);
    fputs("]}\n",stdout);fflush(stdout);
    if(children) XFree(children);
    if(again) XFree(again);
    if(managed) XFree(managed);
    if(stacking) XFree(stacking);
    memset(event_counts,0,sizeof(event_counts));notes_n=overflow=0;
    dirty=followup;notified=0;last_notice=ns();
}
int main(int argc,char **argv) {
    if(argc==2 && !strcmp(argv[1],"--identity")) {
        Display *connection=XOpenDisplay(NULL);
        if(!connection) {fputs("cannot authenticate to selected X display\n",stderr);return 2;}
        struct ucred peer={0};socklen_t len=sizeof(peer);
        struct sockaddr_storage address;socklen_t address_len=sizeof(address);
        int fd=ConnectionNumber(connection);
        int local=!getpeername(fd,(struct sockaddr*)&address,&address_len) && address.ss_family==AF_UNIX;
        int verified=local && !getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&peer,&len);
        printf("{\"local_unix\":%s,\"peer_pid\":%ld}\n",verified?"true":"false",verified?(long)peer.pid:0L);
        XCloseDisplay(connection);return 0;
    }
    unsigned interval=250;
    if(argc==2) { char *end=NULL; unsigned long v=strtoul(argv[1],&end,10);if(!*argv[1] || *end || v<100 || v>10000) return 2;interval=(unsigned)v; }
    else if(argc!=1) return 2;
    d=XOpenDisplay(NULL);if(!d) {fputs("cannot open selected X display\n",stderr);return 2;}
    XSetErrorHandler(on_error);root=DefaultRootWindow(d);began=last_notice=ns();
    intern_existing();XSelectInput(d,root,SubstructureNotifyMask|StructureNotifyMask|PropertyChangeMask);XSync(d,False);
    printf("{\"ready\":true,\"root\":%lu}\n",root);fflush(stdout);
    int done=0;
    while(!done) {
        events();
        uint64_t elapsed=ns()-last_notice;
        if(dirty && !notified && elapsed>= (uint64_t)interval*1000000) {
            fputs("{\"notice\":true}\n",stdout);fflush(stdout);notified=1;
        }
        int timeout=dirty&&!notified ? (int)(interval-elapsed/1000000) : -1;
        if(timeout<0 && dirty&&!notified) timeout=0;
        struct pollfd p[2]={{STDIN_FILENO,POLLIN,0},{ConnectionNumber(d),POLLIN,0}};
        int rc=poll(p,2,timeout);
        if(rc<0) {if(errno==EINTR) continue;break;}
        if(p[0].revents&(POLLIN|POLLHUP)) {
            /* The controller sends one short command at a time; raw read avoids
             * stdio buffering hiding an already-read command from poll. */
            char line[32];ssize_t len=read(STDIN_FILENO,line,sizeof(line)-1);
            if(len<=0) break;
            line[len]='\0';
            if(!strcmp(line,"capture\n")) capture(0);
            else if(!strcmp(line,"refresh\n")) capture(1);
            else if(!strcmp(line,"quit\n")) done=1;
            else {fputs("invalid observer command\n",stderr);break;}
        }
    }
    XCloseDisplay(d);free(nodes);return done?0:2;
}
