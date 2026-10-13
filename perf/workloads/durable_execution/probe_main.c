#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

/* Challenge bytes exist only in process memory and the external oracle stdout. */
static uint64_t nonce, counter, thread_ticks;
static uint64_t *anonymous_state, *file_state;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static int paused, regular_fd, unlinked_fd, pipe_fd[2], child_control[2], child_reply[2];
static pid_t child_pid;

static void fail(const char *operation) { perror(operation); exit(2); }
static void exact_write(int fd, const void *data, size_t length) {
    if (write(fd, data, length) != (ssize_t)length) fail("write");
}

static void *writer(void *unused) {
    (void)unused;
    int log = open("/work/activity.log", O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (log < 0) fail("log");
    for (;;) {
        pthread_mutex_lock(&lock);
        if (!paused) {
            ++counter;
            ++thread_ticks;
            *file_state = counter;
            exact_write(log, &counter, sizeof(counter));
        }
        pthread_mutex_unlock(&lock);
        usleep(50000);
    }
    return NULL;
}

static int connect_control(void) {
    int socket_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    strcpy(address.sun_path, "/work/control.sock");
    if (socket_fd < 0 || connect(socket_fd, (struct sockaddr *)&address, sizeof(address))) fail("connect");
    return socket_fd;
}

static int serve(void) {
    if (getrandom(&nonce, sizeof(nonce), 0) != sizeof(nonce)) fail("nonce");
    anonymous_state = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (anonymous_state == MAP_FAILED) fail("anonymous mmap");
    if (getrandom(anonymous_state, sizeof(*anonymous_state), 0) != sizeof(*anonymous_state)) fail("mmap nonce");
    regular_fd = open("/work/renamed", O_RDWR | O_CREAT | O_EXCL, 0640);
    if (regular_fd < 0) fail("regular");
    exact_write(regular_fd, "fixed-file-cursor", 17);
    if (rename("/work/renamed", "/work/regular") || link("/work/regular", "/work/hardlink")) fail("links");
    if (lseek(regular_fd, 6, SEEK_SET) != 6) fail("regular cursor");
    unlinked_fd = open("/work/unlinked", O_RDWR | O_CREAT | O_EXCL, 0600);
    if (unlinked_fd < 0) fail("unlinked");
    exact_write(unlinked_fd, "deleted-but-open", 16);
    if (lseek(unlinked_fd, 7, SEEK_SET) != 7 || unlink("/work/unlinked")) fail("unlink");
    int mapped = open("/work/mapped", O_RDWR | O_CREAT | O_EXCL, 0600);
    if (mapped < 0 || ftruncate(mapped, 4096)) fail("mapped file");
    file_state = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mapped, 0);
    if (file_state == MAP_FAILED) fail("file mmap");
    if (pipe(pipe_fd) || pipe(child_control) || pipe(child_reply)) fail("pipe");
    exact_write(pipe_fd[1], "pipe-proof", 10);
    child_pid = fork();
    if (child_pid < 0) fail("fork");
    if (child_pid == 0) {
        close(child_control[1]); close(child_reply[0]);
        char command, bytes[10];
        if (read(child_control[0], &command, 1) != 1) _exit(3);
        if (read(pipe_fd[0], bytes, sizeof(bytes)) != sizeof(bytes)) _exit(4);
        exact_write(child_reply[1], bytes, sizeof(bytes));
        for (;;) pause();
    }
    close(child_control[0]); close(child_reply[1]);
    pthread_t thread;
    if (pthread_create(&thread, NULL, writer, NULL)) fail("thread");
    int server = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    strcpy(address.sun_path, "/work/control.sock");
    if (server < 0 || bind(server, (struct sockaddr *)&address, sizeof(address)) || listen(server, 8)) fail("listen");
    for (;;) {
        int client = accept(server, NULL, NULL);
        if (client < 0) fail("accept");
        char request[24] = {0};
        if (read(client, request, sizeof(request) - 1) < 1) fail("request");
        pthread_mutex_lock(&lock);
        if (!strcmp(request, "freeze")) paused = 1;
        if (!strcmp(request, "resume")) paused = 0;
        if (!strcmp(request, "verify")) {
            char bytes[18] = {0}, pipe_bytes[10] = {0};
            exact_write(child_control[1], "v", 1);
            int pipe_ok = read(child_reply[0], pipe_bytes, 10) == 10 && !memcmp(pipe_bytes, "pipe-proof", 10);
            int file_ok = read(regular_fd, bytes, 11) == 11 && !memcmp(bytes, "file-cursor", 11);
            memset(bytes, 0, sizeof(bytes));
            int unlink_ok = read(unlinked_fd, bytes, 9) == 9 && !memcmp(bytes, "-but-open", 9) && access("/work/unlinked", F_OK) == -1;
            struct stat original, hardlink;
            int links_ok = !stat("/work/regular", &original) && !stat("/work/hardlink", &hardlink)
                && original.st_ino == hardlink.st_ino && (original.st_mode & 0777) == 0640;
            dprintf(client, "{\"pipeVerified\":%s,\"fileVerified\":%s,\"unlinkedVerified\":%s,\"linksVerified\":%s}\n",
                pipe_ok ? "true" : "false", file_ok ? "true" : "false", unlink_ok ? "true" : "false", links_ok ? "true" : "false");
        } else {
            int unread = -1;
            if (ioctl(pipe_fd[0], FIONREAD, &unread)) fail("pipe unread");
            dprintf(client, "{\"nonce\":\"%016" PRIx64 "\",\"counter\":%" PRIu64 ",\"mmapNonce\":\"%016" PRIx64 "\",\"fileCounter\":%" PRIu64 ",\"fdOffset\":%ld,\"unlinkedOffset\":%ld,\"pipeBytes\":%d,\"childPid\":%d,\"paused\":%s,\"threadTicks\":%" PRIu64 "}\n",
                nonce, counter, *anonymous_state, *file_state, (long)lseek(regular_fd, 0, SEEK_CUR),
                (long)lseek(unlinked_fd, 0, SEEK_CUR), unread, child_pid, paused ? "true" : "false", thread_ticks);
        }
        pthread_mutex_unlock(&lock);
        close(client);
    }
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (!strcmp(argv[1], "serve")) return serve();
    if (strcmp(argv[1], "observe") && strcmp(argv[1], "freeze") && strcmp(argv[1], "resume") && strcmp(argv[1], "verify")) return 2;
    int client = connect_control();
    exact_write(client, argv[1], strlen(argv[1]));
    shutdown(client, SHUT_WR);
    char bytes[4096];
    ssize_t count;
    while ((count = read(client, bytes, sizeof(bytes))) > 0) exact_write(STDOUT_FILENO, bytes, (size_t)count);
    if (count < 0) fail("response");
    close(client);
    return 0;
}
