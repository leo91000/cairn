# Boot without unused physical-device probes

Date: 2026-10-01.

## Problem

Fresh production conversations still need 9–10 seconds before their first model
turn. The measured Firecracker boot phase accounts for about three seconds.
An isolated reproduction starts the same 7-vCPU, 35.5-GiB guest with a new 32-GiB
disk and executes one Node readiness message, without Codex, authentication or a
model request. Kernel timestamps show a roughly half-second wait for an emulated
PS/2 keyboard that an agent never uses.

## Decision

Guest boot arguments include `i8042.nokbd i8042.noaux`. The outer Firecracker
guest has virtio storage/networking and a serial console, with no physical
keyboard or mouse. Nested Android virtual machines keep their own device model
and boot arguments.

Set `loglevel=5` to retain warnings and errors on the serial console while
avoiding informational output during boot. Informational messages remain in
the guest kernel log, accessible through `dmesg`; the host still records the
guest console and VMM log. Resource limits, the retained root image, kernel
selection, entropy, security mitigations, isolation, synchronization and
shutdown protections are unchanged.

## Evidence and limits

The disposable VM reproduction on the production host varies only the appended
boot arguments. After each runner's initial image extraction, measured VM boot
times are:

| Arguments | Boot samples |
| --- | --- |
| Existing arguments | 3,208 / 3,181 ms |
| Warnings/errors only | 2,935 / 2,943 ms |
| No keyboard/mouse probe | 2,467 / 2,469 ms |
| Both changes | 2,066 / 2,142 ms |

On the development host the combined change boots in 510–547 ms, compared with
1,051–1,170 ms. The raw samples include initial image extraction and filesystem
preparation separately; these results are not end-to-end production deployment
evidence or a latency guarantee. A network-configuration-delay hypothesis was
not retained: the shipped upstream IP configuration code only delays ten
milliseconds after opening its network device.

The actual runner smoke test checks that the guest has no PS/2 keyboard, then
exercises networking, the authentication relay, isolation, workspace recovery,
nested KVM and Docker. Its baseline fails on the keyboard assertion. Release
qualification additionally covers repeated storage publication, crash recovery
and Intel Android first launch/resume on the exact release image.

The runner smoke fixture defaults to `/var/tmp`, consistently with the Android
and storage soak fixtures. On hosts where `/tmp` is a RAM filesystem, extracting
the 5.2-GiB guest image there exhausts the smoke container's 6-GiB memory limit
and kills the VMM. The unchanged published image reproduced that failure too.
Using disk-backed fixture storage preserves the memory limit and avoids
confusing test-filesystem exhaustion with a VM boot regression.

Raw comparisons: [guest-boot-2026-10-01.json](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/guest-boot-2026-10-01.json).
Parameter definitions: [upstream Linux documentation](https://www.kernel.org/doc/html/v6.12/admin-guide/kernel-parameters.html).
