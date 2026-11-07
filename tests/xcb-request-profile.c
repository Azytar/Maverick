#define _GNU_SOURCE
/* Test-only interposer for x11rb's RAW request submission boundary. It does
 * not parse sockets, count flushes, or claim server completion/latency. The
 * native client remains the independent focus/appearance oracle. */
#include <xcb/xcbext.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static _Thread_local unsigned depth;
static int output = -1;

__attribute__((constructor)) static void start(void) {
    const char *path = getenv("MAVERICK_XCB_PROFILE_PATH");
    if (!path) return;
    output = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (output < 0) { perror("xcb-request-profile"); _exit(2); }
}

static void capture(const unsigned char *bytes, uint64_t sequence) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) _exit(2);
    uint32_t target, detail, value;
    memcpy(&target, bytes + 4, 4);
    memcpy(&detail, bytes + 8, 4);
    memcpy(&value, bytes + 12, 4);
    if (dprintf(output, "{\"ns\":%" PRIu64 ",\"sequence\":%" PRIu64
                ",\"opcode\":%u,\"target\":%u,\"detail\":%u,\"value\":%u}\n",
                (uint64_t)now.tv_sec * 1000000000 + (uint64_t)now.tv_nsec,
                sequence, bytes[0], target, detail, value) < 0) _exit(2);
}

static int header(int flags, const struct iovec *vectors,
                  const xcb_protocol_request_t *request, unsigned char *bytes) {
    if (output < 0 || depth || !(flags & XCB_REQUEST_RAW)) return 0;
    size_t copied = 0;
    memset(bytes, 0, 16);
    for (size_t i = 0; i < request->count && copied < 16; i++) {
        size_t length = vectors[i].iov_len;
        if (length > 16 - copied) length = 16 - copied;
        if (vectors[i].iov_base) memcpy(bytes + copied, vectors[i].iov_base, length);
        copied += length;
    }
    return copied >= 4;
}

uint64_t xcb_send_request64(xcb_connection_t *connection, int flags,
                          struct iovec *vectors, const xcb_protocol_request_t *request) {
    typedef uint64_t (*Function)(xcb_connection_t *, int, struct iovec *, const xcb_protocol_request_t *);
    static Function next;
    if (!next) {
        void *symbol = dlsym(RTLD_NEXT, "xcb_send_request64");
        _Static_assert(sizeof(next) == sizeof(symbol), "Linux function pointer representation");
        memcpy(&next, &symbol, sizeof(next));
        if (!next) _exit(2);
    }
    unsigned char bytes[16];
    int record = header(flags, vectors, request, bytes);
    depth++;
    uint64_t sequence = next(connection, flags, vectors, request);
    depth--;
    if (record) capture(bytes, sequence);
    return sequence;
}

uint64_t xcb_send_request_with_fds64(xcb_connection_t *connection, int flags,
                                   struct iovec *vectors, const xcb_protocol_request_t *request,
                                   unsigned count, int *fds) {
    typedef uint64_t (*Function)(xcb_connection_t *, int, struct iovec *, const xcb_protocol_request_t *, unsigned, int *);
    static Function next;
    if (!next) {
        void *symbol = dlsym(RTLD_NEXT, "xcb_send_request_with_fds64");
        _Static_assert(sizeof(next) == sizeof(symbol), "Linux function pointer representation");
        memcpy(&next, &symbol, sizeof(next));
        if (!next) _exit(2);
    }
    unsigned char bytes[16];
    int record = header(flags, vectors, request, bytes);
    depth++;
    uint64_t sequence = next(connection, flags, vectors, request, count, fds);
    depth--;
    if (record) capture(bytes, sequence);
    return sequence;
}
