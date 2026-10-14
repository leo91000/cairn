// Keep one L2 VM and vCPU alive across suspension of its Firecracker parent.
#include <errno.h>
#include <fcntl.h>
#include <linux/kvm.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

static void require(int ok, const char *operation) {
    if (!ok) {
        fprintf(stderr, "%s: %s\n", operation, strerror(errno));
        exit(1);
    }
}

int main(void) {
    require(getuid() == 1000, "probe must run as the agent");
    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    require(kvm >= 0, "open nested KVM");
    require(ioctl(kvm, KVM_GET_API_VERSION, 0) == 12, "KVM API version");
    int vm = ioctl(kvm, KVM_CREATE_VM, 0);
    require(vm >= 0, "create nested VM");
    require(ioctl(vm, KVM_SET_TSS_ADDR, 0xfffbd000) == 0, "set TSS address");
    unsigned char *memory = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(memory != MAP_FAILED, "allocate nested memory");
    // mov ax,42; increment ax; emit it through port 0x23; repeat forever.
    memcpy(memory, "\xb8\x2a\x00\x40\xe7\x23\xeb\xfb", 8);
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0x1000, .memory_size = 4096,
        .userspace_addr = (unsigned long)memory,
    };
    require(ioctl(vm, KVM_SET_USER_MEMORY_REGION, &region) == 0, "map nested memory");
    int cpu = ioctl(vm, KVM_CREATE_VCPU, 0);
    require(cpu >= 0, "create nested vCPU");
    int size = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    require(size >= (int)sizeof(struct kvm_run), "vCPU mmap size");
    struct kvm_run *run = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, cpu, 0);
    require(run != MAP_FAILED, "map nested vCPU");
    struct kvm_sregs special;
    require(ioctl(cpu, KVM_GET_SREGS, &special) == 0, "read segments");
    special.cs.base = 0;
    special.cs.selector = 0;
    require(ioctl(cpu, KVM_SET_SREGS, &special) == 0, "set segments");
    struct kvm_regs registers = {.rip = 0x1000, .rflags = 2};
    require(ioctl(cpu, KVM_SET_REGS, &registers) == 0, "set registers");
    unsigned long long expected = 43;
    for (;;) {
        int result;
        do {
            result = ioctl(cpu, KVM_RUN, 0);
        } while (result < 0 && errno == EINTR);
        require(result == 0, "execute retained nested guest");
        require(run->exit_reason == KVM_EXIT_IO, "nested guest must emit its counter");
        require(ioctl(cpu, KVM_GET_REGS, &registers) == 0, "read retained counter");
        require(registers.rax == expected++, "nested register state must advance once");
        printf("nested-live pid=%d value=%llu\n", getpid(), registers.rax);
        fflush(stdout);
        usleep(100000);
    }
}
