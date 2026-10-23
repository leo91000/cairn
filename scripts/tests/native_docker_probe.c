/* Identical local host/container volume workload; no network or privileged IO. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <time.h>
#include <unistd.h>

static void require(int success) {
    if (!success) { perror("native Docker probe"); exit(1); }
}
static double milliseconds(clockid_t clock) {
    struct timespec t; require(clock_gettime(clock, &t) == 0);
    return t.tv_sec * 1000.0 + t.tv_nsec / 1000000.0;
}
int main(int argc, char **argv) {
    require(argc == 3); require(chdir(argv[2]) == 0);
    if (!strcmp(argv[1], "metadata-create")) {
        int fd = open("capability", O_CREAT | O_WRONLY | O_TRUNC, 0600); require(fd >= 0);
        require(write(fd, "fixture", 7) == 7); require(close(fd) == 0);
        require(chmod("capability", 06750) == 0);
        uint32_t capability[5] = {0x02000001, 1 << 10, 0, 0, 0};
        require(setxattr("capability", "security.capability", capability, sizeof(capability), 0) == 0);
    } else if (!strcmp(argv[1], "metadata-check")) {
        struct stat info; require(stat("capability", &info) == 0);
        require((info.st_mode & 07777) == 06750);
        uint32_t capability[6] = {0};
        ssize_t size = getxattr("capability", "security.capability", capability, sizeof(capability));
        require(size >= 20 && capability[1] == (1 << 10));
        puts("metadata-preserved");
    } else {
        unsigned char buffer[4096];
        for (unsigned i = 0; i < sizeof(buffer); i++) buffer[i] = (i * 37 + 11) & 255;
        int fd = open("asset", O_CREAT | O_RDWR | O_TRUNC, 0600); require(fd >= 0);
        for (int i = 0; i < 2048; i++) require(write(fd, buffer, sizeof(buffer)) == sizeof(buffer));
        require(fsync(fd) == 0);
        double cold = 0, warm = 0, cold_cpu = 0, warm_cpu = 0;
        for (int phase = 0; phase < 2; phase++) {
            require(lseek(fd, 0, SEEK_SET) == 0);
            if (phase == 0) require(posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED) == 0);
            double wall = milliseconds(CLOCK_MONOTONIC), cpu = milliseconds(CLOCK_PROCESS_CPUTIME_ID);
            for (int i = 0; i < 2048; i++) require(read(fd, buffer, sizeof(buffer)) == sizeof(buffer));
            cpu = milliseconds(CLOCK_PROCESS_CPUTIME_ID) - cpu; wall = milliseconds(CLOCK_MONOTONIC) - wall;
            if (phase) { warm = wall; warm_cpu = cpu; } else { cold = wall; cold_cpu = cpu; }
        }
        require(close(fd) == 0);
        fd = open("writes", O_CREAT | O_WRONLY | O_TRUNC, 0600); require(fd >= 0);
        double wall = milliseconds(CLOCK_MONOTONIC), cpu = milliseconds(CLOCK_PROCESS_CPUTIME_ID);
        for (int i = 0; i < 128; i++) { require(write(fd, buffer, sizeof(buffer)) == sizeof(buffer)); require(fsync(fd) == 0); }
        cpu = milliseconds(CLOCK_PROCESS_CPUTIME_ID) - cpu; wall = milliseconds(CLOCK_MONOTONIC) - wall;
        require(close(fd) == 0);
        printf("{\"readBytes\":8388608,\"writeBytes\":524288,\"coldMs\":%.3f,\"warmMs\":%.3f,\"writeMs\":%.3f,\"coldCpuMs\":%.3f,\"warmCpuMs\":%.3f,\"writeCpuMs\":%.3f}\n", cold, warm, wall, cold_cpu, warm_cpu, cpu);
    }
    return 0;
}
