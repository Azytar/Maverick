#define _GNU_SOURCE
#include <X11/Xlib.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

static void forbidden(void) {fputs("PASSIVITY GUARD REJECTED A MUTATING/CONTENT CALL\n",stderr);abort();}
int XSelectInput(Display *d, Window w, long mask) {
    if(mask & ~(SubstructureNotifyMask|StructureNotifyMask|PropertyChangeMask)) forbidden();
    int (*real)(Display*,Window,long)=dlsym(RTLD_NEXT,"XSelectInput");
    return real(d,w,mask);
}
Status XInternAtoms(Display *d,char **names,int n,Bool only,Atom *atoms) {
    if(!only) forbidden();
    Status (*real)(Display*,char**,int,Bool,Atom*)=dlsym(RTLD_NEXT,"XInternAtoms");
    return real(d,names,n,only,atoms);
}
/* The public APIs used by a mutating observer are intercepted. This is a
 * dynamic guard of this implementation, not a sandbox for arbitrary Xlib code. */
#define BLOCK_INT(name, args) int name args {forbidden();return 0;}
BLOCK_INT(XSetInputFocus,(Display*d,Window w,int r,Time t))
BLOCK_INT(XConfigureWindow,(Display*d,Window w,unsigned m,XWindowChanges*c))
BLOCK_INT(XMapWindow,(Display*d,Window w))
BLOCK_INT(XUnmapWindow,(Display*d,Window w))
BLOCK_INT(XRaiseWindow,(Display*d,Window w))
BLOCK_INT(XLowerWindow,(Display*d,Window w))
BLOCK_INT(XRestackWindows,(Display*d,Window*w,int n))
BLOCK_INT(XChangeProperty,(Display*d,Window w,Atom p,Atom t,int f,int m,const unsigned char*b,int n))
BLOCK_INT(XDeleteProperty,(Display*d,Window w,Atom p))
BLOCK_INT(XReparentWindow,(Display*d,Window w,Window p,int x,int y))
Window XCreateWindow(Display*d,Window p,int x,int y,unsigned w,unsigned h,unsigned b,int dep,unsigned c,Visual*v,unsigned long m,XSetWindowAttributes*a) {forbidden();return 0;}
Window XCreateSimpleWindow(Display*d,Window p,int x,int y,unsigned w,unsigned h,unsigned b,unsigned long border,unsigned long bg) {forbidden();return 0;}
XImage *XGetImage(Display*d,Drawable w,int x,int y,unsigned width,unsigned height,unsigned long planes,int format) {forbidden();return NULL;}
Status XFetchName(Display*d,Window w,char**name) {forbidden();return 0;}
