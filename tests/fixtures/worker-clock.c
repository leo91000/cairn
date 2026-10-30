// Test-only clock for the disposable worker process. Monotonic time remains
// real so network timeouts and test deadlock guards continue to work.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

int clock_gettime(clockid_t clock, struct timespec *time) {
    const char *path = getenv("CAIRN_FIXTURE_CLOCK_FILE");
    if (clock == CLOCK_REALTIME && path != NULL) {
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd >= 0) {
            char value[32];
            ssize_t length = read(fd, value, sizeof(value) - 1);
            close(fd);
            if (length > 0) {
                value[length] = '\0';
                char *end;
                long long millis = strtoll(value, &end, 10);
                if (*end == '\0' && millis >= 0) {
                    time->tv_sec = millis / 1000;
                    time->tv_nsec = (millis % 1000) * 1000000;
                    return 0;
                }
            }
        }
    }
    return syscall(SYS_clock_gettime, clock, time);
}

int gettimeofday(struct timeval *time, void *zone) {
    if (getenv("CAIRN_FIXTURE_CLOCK_FILE") != NULL) {
        struct timespec now;
        if (clock_gettime(CLOCK_REALTIME, &now) == 0) {
            time->tv_sec = now.tv_sec;
            time->tv_usec = now.tv_nsec / 1000;
            return 0;
        }
    }
    return syscall(SYS_gettimeofday, time, zone);
}
