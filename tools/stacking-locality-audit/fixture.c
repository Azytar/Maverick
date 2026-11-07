#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static Display *d;static Window root,w[16],external,dock,exclusive;static unsigned count;
static Window create(const char *class_name,Window transient,int redirect) {
    XSetWindowAttributes a={0};a.override_redirect=redirect;a.background_pixel=0x557799;
    Window id=XCreateWindow(d,root,(int)count*40,40,240,200,0,CopyFromParent,InputOutput,CopyFromParent,CWOverrideRedirect|CWBackPixel,&a);
    XClassHint c={(char*)"LOCALITY_PRIVATE_INSTANCE",(char*)class_name};XSetClassHint(d,id,&c);
    XStoreName(d,id,"LOCALITY_PRIVATE_TITLE_DO_NOT_PERSIST");
    XWMHints hints={0};hints.flags=InputHint;hints.input=True;XSetWMHints(d,id,&hints);
    if(transient)XSetTransientForHint(d,id,transient);
    return id;
}
static void state(Window id,const char *name,int on) {
    XEvent e={0};e.xclient.type=ClientMessage;e.xclient.window=id;e.xclient.message_type=XInternAtom(d,"_NET_WM_STATE",False);e.xclient.format=32;e.xclient.data.l[0]=on;e.xclient.data.l[1]=(long)XInternAtom(d,name,False);e.xclient.data.l[3]=2;
    XSendEvent(d,root,False,SubstructureRedirectMask|SubstructureNotifyMask,&e);
}
int main(void) {
    d=XOpenDisplay(NULL);if(!d)return 2;root=DefaultRootWindow(d);char line[64];
    puts("{\"fixture_ready\":true}");fflush(stdout);
    while(fgets(line,sizeof(line),stdin)) {
        if(!strcmp(line,"quit\n"))break;
        if(!strcmp(line,"tiles\n")) {for(unsigned i=0;i<5;i++){w[count]=create("LocalityTile",0,False);XMapWindow(d,w[count]);count++;}}
        else if(!strcmp(line,"float\n")) {w[count]=create("LocalityFloat",0,False);XMapWindow(d,w[count]);count++;}
        else if(!strcmp(line,"dialog\n")) {w[count]=create("LocalityDialog",w[0],False);XMapWindow(d,w[count]);count++;}
        else if(!strcmp(line,"or\n")) {external=create("LocalityMenu",0,True);XMapWindow(d,external);}
        else if(!strcmp(line,"between\n")) {XWindowChanges c={0};c.sibling=w[0];c.stack_mode=Above;XConfigureWindow(d,external,CWSibling|CWStackMode,&c);}
        else if(!strcmp(line,"or-top\n"))XRaiseWindow(d,external);
        else if(!strcmp(line,"dock\n")) {dock=create("LocalityDock",0,False);Atom type=XInternAtom(d,"_NET_WM_WINDOW_TYPE_DOCK",False);XChangeProperty(d,dock,XInternAtom(d,"_NET_WM_WINDOW_TYPE",False),XA_ATOM,32,PropModeReplace,(unsigned char*)&type,1);unsigned long strut[12]={0,0,24,0,0,0,0,0,0,1279,0,0};XChangeProperty(d,dock,XInternAtom(d,"_NET_WM_STRUT_PARTIAL",False),XA_CARDINAL,32,PropModeReplace,(unsigned char*)strut,12);XMapWindow(d,dock);}
        else if(!strcmp(line,"exclusive\n")) {exclusive=create("LocalityExclusive",0,False);XMapWindow(d,exclusive);state(exclusive,"_NET_WM_STATE_FULLSCREEN",1);}
        else if(!strcmp(line,"exclusive-restore\n")) {state(exclusive,"_NET_WM_STATE_FULLSCREEN",0);XDestroyWindow(d,exclusive);exclusive=0;}
        else if(!strcmp(line,"activate\n")) {XEvent e={0};e.xclient.type=ClientMessage;e.xclient.window=w[0];e.xclient.message_type=XInternAtom(d,"_NET_ACTIVE_WINDOW",False);e.xclient.format=32;e.xclient.data.l[0]=2;XSendEvent(d,root,False,SubstructureRedirectMask|SubstructureNotifyMask,&e);}
        else if(!strcmp(line,"fullscreen\n"))state(w[0],"_NET_WM_STATE_FULLSCREEN",1);
        else if(!strcmp(line,"restore\n"))state(w[0],"_NET_WM_STATE_FULLSCREEN",0);
        else if(!strcmp(line,"maximize\n")) {state(w[0],"_NET_WM_STATE_MAXIMIZED_HORZ",1);state(w[0],"_NET_WM_STATE_MAXIMIZED_VERT",1);}
        else if(!strcmp(line,"unmaximize\n")) {state(w[0],"_NET_WM_STATE_MAXIMIZED_HORZ",0);state(w[0],"_NET_WM_STATE_MAXIMIZED_VERT",0);}
        else if(!strcmp(line,"unmap\n"))XUnmapWindow(d,w[4]);
        else if(!strcmp(line,"remap\n"))XMapWindow(d,w[4]);
        else if(!strcmp(line,"destroy\n")) {XDestroyWindow(d,w[4]);w[4]=0;}
        else if(!strcmp(line,"reparent\n"))XReparentWindow(d,external,w[1],0,0);
        else if(!strcmp(line,"root-again\n"))XReparentWindow(d,external,root,10,10);
        else if(!strcmp(line,"order\n")) {Window order[3]={w[2],w[1],w[0]};XRestackWindows(d,order,3);}
        else if(!strcmp(line,"bad-state\n")) {unsigned char invalid=0;XChangeProperty(d,w[0],XInternAtom(d,"_NET_WM_STATE",False),XA_STRING,8,PropModeReplace,&invalid,1);}
        else if(!strcmp(line,"good-state\n"))XDeleteProperty(d,w[0],XInternAtom(d,"_NET_WM_STATE",False));
        else {fputs("invalid fixture command\n",stderr);return 2;}
        XSync(d,False);printf("{\"ok\":true,\"tiles\":[");
        for(unsigned i=0;i<5 && i<count;i++)printf("%s%lu",i?",":"",w[i]);
        printf("],\"external\":%lu,\"dock\":%lu}\n",external,dock);fflush(stdout);
    }
    XCloseDisplay(d);return 0;
}
