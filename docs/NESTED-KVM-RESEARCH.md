# Nested KVM inside the Firecracker guest

Research date: 2026-09-25. Scope: the pinned Firecracker **v1.17.0** (`95f868c8e345b1cc8faccd1a3c910b4989dc3f58`) and Linux **6.12.109**, x86_64 Intel and AMD. These are source findings and implementation recommendations, **not evidence of a successful nested boot**.

## Feasibility and the missing VMX flag

The implementation is feasible in principle without patching Firecracker or inventing a CPU template. Firecracker initializes guest CPUID from `KVM_GET_SUPPORTED_CPUID`; the default Intel/AMD normalization preserves VMX/SVM. This is an inference from the pinned source, not an upstream support guarantee. The repository currently selects no CPU template. [KVM initialization](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/arch/x86_64/kvm.rs), [CPU configuration](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/arch/x86_64/mod.rs), [Intel normalization](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/cpu_config/x86_64/cpuid/intel/normalize.rs), [AMD normalization](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/cpu_config/x86_64/cpuid/amd/normalize.rs).

**A missing `vmx` in the current guest's `/proc/cpuinfo` does not prove that the outer host hides VMX.** Linux 6.12.109 only enables VMX in the initially unlocked `IA32_FEAT_CTL` register when `CONFIG_KVM_INTEL` is enabled; otherwise it locks the register without enabling VMX and clears the reported CPU capability. The current minimal guest configuration does not request KVM. [Linux feature-control initialization](https://github.com/gregkh/linux/blob/v6.12.109/arch/x86/kernel/cpu/feat_ctl.c#L104-L172), [upstream x86_64 default configuration](https://github.com/gregkh/linux/blob/v6.12.109/arch/x86/configs/x86_64_defconfig).

## Required configuration

1. **Outer host:** hardware virtualization available, outer `/dev/kvm` usable by the runner, and `/sys/module/kvm_intel/parameters/nested` or `/sys/module/kvm_amd/parameters/nested` enabled. Linux defaults to enabling nesting on recent kernels, but distributions may override it. If the outer host is itself a VM, its provider must expose nesting too. Check the actual host; do not unload a KVM module while other VMs run. [Linux nested-guest guide](https://www.kernel.org/doc/html/latest/virt/kvm/x86/running-nested-guests.html).
2. **Guest kernel:** explicitly build `CONFIG_VIRTUALIZATION=y`, `CONFIG_KVM=y`, `CONFIG_KVM_INTEL=y`, and `CONFIG_KVM_AMD=y`. Assert these in the final generated `.config`, not merely in the fragment. Modules are disabled in this image, so installing module packages or running `modprobe` cannot repair an already booted guest. Intel requires `IA32_FEAT_CTL`; AMD requires AMD/Hygon CPU support, normally supplied by x86_64 defaults. [Pinned Linux KVM configuration](https://github.com/gregkh/linux/blob/v6.12.109/arch/x86/kvm/Kconfig).
3. **Guest device:** devtmpfs creates the guest's own `/dev/kvm` after successful KVM initialization. Grant read/write access to execution GID 1000, for example ownership `root:1000` and mode `0660`, during guest init when the character device exists. This repository explicitly clears supplementary groups before switching to UID/GID 1000, so adding `node` to a separate `kvm` group alone would not suffice. Do not bind the outer host's device into the guest. Repository evidence: `deploy/microvm/init` and the credential switches in `crates/installation/src/microvm/guest.rs`.

## CPUID and MSRs

Intel advertises VMX in CPUID leaf 1 ECX bit 5; AMD advertises SVM in leaf `0x80000001` ECX bit 2 and its capabilities in leaf `0x8000000a`. Outer KVM's vendor code gates exposure on its nesting setting. Preserve its supported CPUID rather than forcibly setting unsupported bits. [Intel KVM capabilities](https://github.com/gregkh/linux/blob/v6.12.109/arch/x86/kvm/vmx/vmx.c), [AMD KVM capabilities](https://github.com/gregkh/linux/blob/v6.12.109/arch/x86/kvm/svm/svm.c).

No custom MSR override appears necessary for a fresh Linux boot: Firecracker's boot MSRs do not overwrite Intel feature-control or AMD virtualization controls; KVM handles their guest semantics. Guest Linux initializes Intel feature-control itself when built with KVM. Avoid hard-coding VMX control MSRs, which depend on the host. This is a source-derived recommendation that still requires execution on hardware. [Firecracker boot MSRs](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/arch/x86_64/msr.rs#L392-L430).

Do not introduce static C3/T2/T2A/T2CL/T2S templates: these intentionally mask virtualization features. Custom templates can also silently request bits KVM rejects; they cannot manufacture missing virtualization hardware. [Pinned template documentation](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/cpu_templates/cpu-templates.md), [Intel example](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/cpu_config/x86_64/static_cpu_templates/t2.rs), [AMD example](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/cpu_config/x86_64/static_cpu_templates/t2a.rs).

## Pool and limitations

The existing pool boots a fresh anonymous VM, warms it, and pauses that same live process; it does not serialize or clone Firecracker snapshots. Adoption resumes that process, while later conversations boot from persistent disk. This is compatible with avoiding nested-state snapshot restoration. Repository evidence: `crates/installation/src/microvm/pool.rs` and `crates/installation/src/microvm/host.rs`.

Upstream explicitly says **“Firecracker is not tested with nested virtualization.”** Its MSR dump excludes VMX capability registers, and its serialized vCPU state has no KVM nested-state field. Do not claim live snapshot/migration support for running nested VMs. These findings do not prohibit fresh boot or pause/resume of the same process. [Upstream limitation and dump filtering](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/arch/x86_64/msr.rs#L298-L321), [serialized vCPU state](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/arch/x86_64/vcpu.rs#L792-L823).

## Acceptance evidence

- On the actual outer host, record CPU vendor, kernel, Firecracker version, and nested setting without changing a running hypervisor's modules.
- Inside a newly built guest **as UID 1000**, assert the CPU capability, open `/dev/kvm`, check API version 12, create a VM/vCPU and execute a tiny guest through `KVM_RUN`. Mere device existence or VM creation is insufficient evidence of execution. [KVM interface](https://www.kernel.org/doc/html/latest/virt/kvm/api.html).
- Exercise both a cold guest and adoption of the prepared pool guest; stop cleanly and prove execution in a subsequent fresh guest.
- Run Android Emulator with acceleration required, check its acceleration diagnostic, wait for Android boot, and execute the existing instrumentation test. A software fallback must fail this acceptance check rather than masquerade as successful acceleration.
- Report the CPU vendor actually tested. Passing Intel does not establish AMD runtime compatibility, or vice versa. Keep a clear unavailable result when the outer host cannot supply nesting.

## Runtime evidence, 2026-09-25

The [image integration job](https://github.com/leo91000/cairn/actions/runs/36142279729) successfully executed `nested-kvm: agent uid=1000, KVM_RUN, rax=42, HLT` inside Firecracker. The runner disk export/delete/import and subsequent guest execution also passed. This establishes nested execution, rather than only the presence of `/dev/kvm`.

The [instrumented Android probe](https://github.com/leo91000/cairn/actions/runs/36144997216) identified an AMD EPYC guest CPU and confirmed `Nested Virtualization enabled` and `Nested Paging enabled` in the guest kernel. Android reached userspace initialization before the 180-second helper deadline stopped it. This run did **not** establish a completed Android boot. These hosted runs do not establish Intel runtime behavior.


### Intel host comparison and release status

An isolated probe on an Intel Xeon E5-1410 v2 host (Linux 6.8.0-111-generic, `kvm_intel.nested=Y`) also passed the UID 1000 `KVM_RUN` check inside Firecracker. With the candidate 600-second launcher deadline, Android API 34 reported `state=ready`, `acceleration=kvm`; one emulator log measured boot at 283,243 ms. This proves a completed accelerated Android boot on that host, **not** a successful application test.

The application interaction subsequently failed: the expected button did not appear, and a later probe exposed Android's `Process system isn't responding` dialog. No nested Android interaction or device-state persistence success is claimed. The host hypervisor configuration was not changed, and the production containers were not restarted for these isolated probes.

The helper deadline was increased to ten minutes based on the measured boot, while the integration test still requires an actual button tap and persisted state in a fresh Firecracker guest. System and crash logs from the disposable Android test device are now collected on failure. The required nested Android release check remains enabled; KVM instruction execution alone cannot satisfy it.

A control test on the **same Intel host**, using the same API 34 fixture with KVM directly in a disposable Docker container (4 GiB RAM, two CPUs), passed: boot, APK installation, button discovery, tap and `Device test passed`. This rules out a consistently broken synthetic APK. It does not substitute for the required nested test. Setup plus boot took 249,210 ms, including the SDK and system-image installation; this duration is not an isolated boot benchmark.

The [candidate image CI](https://github.com/leo91000/cairn/actions/runs/36153618751) for commit `dee77c4` passed code quality, all three browser suites and the container/Firecracker smoke tests. Its exact published image (`sha256:e3140e18bc8d1ebb36650a37b2fc50573589af6405d7e281908770c3744aa3fb`) still failed API 34 boot at the real ten-minute deadline. No copied helper or diagnostic launch override was used in this CI run. The release must not be described as validated based on the other passing jobs.

### AOSP comparison

The API 34 AOSP variant subsequently **passed both first and resumed runs inside Firecracker on Intel**, with KVM required and actual button interaction. Setup plus first boot took 247,999 ms; the complete first probe took 328,895 ms. Setup plus resumed boot took 122,013 ms; the complete resumed probe took 148,184 ms. The first run required one explicit wait on an Android system dialog; the resumed run required none. Screenshots confirmed `Device test passed` in both runs. This comparison used the candidate launcher staged into the older immutable image, so validation of the final shipped image remains necessary.

The launcher exposes this as `--aosp`, retaining Google APIs as the existing default and using a distinct AVD name. This choice follows the measured system-image difference, not a claim that KVM guarantees every Android image works. Official [AOSP image documentation](https://developer.android.com/studio/run/managing-avds) confirms that these images exclude Google apps/services. The [API 29 Google APIs comparison on the exact candidate image](https://github.com/leo91000/cairn/actions/runs/36156560603) also failed; lowering the Android API alone did not solve that hosted run.

### Qualification on the existing Intel host

The unmodified candidate image `sha256:b3ee8b575e66dfc31a530da3e79a0888296cc528c62acf120e2087d8827882a2` subsequently passed both Intel AOSP runs: first interaction 282,514 ms (including setup), persistent-device restart 135,061 ms. The launcher was not injected or replaced. The later hosted AMD AOSP run still timed out after 600 seconds. The cause of that platform difference has not been established.

Release qualification therefore runs this same bounded, disposable probe on Intel separately from the GitHub-hosted build. A trusted operator records success against the exact commit and full image digest, with a durable evidence URL; publication waits for that status and the ordinary CI checks. No permanent runner or CI Docker access is installed on the production host. The hosted nested workflow remains available as a diagnostic. See [the release procedure](DEPLOYMENT.md#intel-qualification-before-publishing-an-image).
