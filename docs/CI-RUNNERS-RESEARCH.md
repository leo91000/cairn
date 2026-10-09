# Faster CI runners for a five-minute PR pipeline

Research date: 2026-10-04. Scope: paid or self-hosted runners that could bring pull-request CI from today's **16.0 min median** down to about **5 min**, for this **public** repository owned by the **personal** account `leo91000`. Prices exclude VAT and come from vendor pages fetched on that date. Nothing was signed up for or tried. The speedups are **estimates**, not measurements. Every option still needs the go/no-go probe described below.

## Answer

- **No runner gets PR CI to 5 min by hardware alone.** The image job's critical step is a single-crate Rust release compile (`codegen-units = 1`, thin LTO) that is mostly serial, followed by serial Firecracker smoke tests. A fast runner with a persistent local Docker cache should bring the critical path to about **7–8 min**. Reaching about **5 min** also needs three workflow changes (see [Speedup reasoning](#speedup-reasoning-for-the-image-job)).
- **Recommendation: Namespace**, after a probe on its trial. It is the only *managed* option that documents personal-account installation, nested virtualization on `linux/amd64`, NVMe cache volumes and cache writes restricted to `main`. Estimated cost is **~$265/month** at the measured volume if job durations drop as estimated, or **~$485/month** if they don't. The Team plan is needed for 64-vCPU concurrency.
- **Cheapest plausible option: RunsOn on AWS `c8i` spot**, at **~$70–100/month** plus AWS operations. It is Intel only, and AWS documents nesting to L2 while the image job needs L3.
- **Owner's OVH hardware: a Rise-L bare-metal server at €149.99/month.** It has the fastest single-thread CPU in this comparison and standard nesting depth. However, GitHub says self-hosted runners "should almost never be used for public repositories", and it would need custom per-job VM isolation.
- **None of these three requires converting to an organization.** Blacksmith, WarpBuild, Depot runners, GitHub larger runners and actuated do.

## Current usage

Method: list every run of each workflow created between 2026-09-20 04:25 and 2026-10-04 19:21 UTC (14.62 days) with `gh api repos/leo91000/leo-agent-manager/actions/workflows/<file>/runs?created=>=2026-09-20`. Then fetch each run's jobs with `actions/runs/<id>/jobs?filter=all`, so re-run attempts are included. Job minutes are `completed_at − started_at`, rounded up per job the way paid GitHub runners are billed. Skipped jobs are excluded. Cancelled and failed jobs are included, because they would be billed on a paid runner. Results are scaled ×30/14.62.

The window contains 809 runs (491 `ci.yaml`, 295 `android.yaml`) and 3,877 job executions, all on `ubuntu-latest`. There were 55 PRs and 362 commits.

| Job (workflow) | Jobs / 30 d | Avg min (all outcomes) | Median min (successful PR jobs since 09-28) | Runner-min / 30 d |
| --- | ---: | ---: | ---: | ---: |
| `image` (ci) | 973 | 15.6 | 14.9 (main 19.1) | 15,376 |
| `quality` (ci) | 973 | 5.4 | 8.2 | 5,616 |
| `browser` ×4 (ci) | 2,887 | 4.0 | 4.3 | 12,540 |
| `resolve`, `publish`, `deploy` (ci) | 2,032 | — | 0.1–2.5 | 2,856 |
| `check` (android) | 562 | 11.4 | 11.6 | 6,687 |
| `device-extra` (android) | 228 | 9.5 | 9.7 | 2,200 |
| `release`, `resolve`, `publish` (android) | 959 | — | 0.1–4.2 | 2,267 |
| Other (CLI updates, nested probe, since-removed jobs) | — | — | — | 1,154 |
| **Total** | | | | **48,693** |

The last seven days run about 20% higher (**59,400 min/30 d**: image 17,477, quality 8,713, browser 16,569). Successful runs since 09-28 had these elapsed times:

- PR: median 16.0 min, p90 23.5 min.
- main: median 20.8 min.
- Android PR: median 12.1 min.

Today this costs nothing: standard hosted runners are "free and unlimited on public repositories" ([GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)).

**Keep `resolve` on free runners in every option.** On tags it can poll for main for up to 40 minutes, and a paid runner would bill that wait.

## Where the image job spends its time

Example: the warm PR [image job of run 37221466077](https://github.com/leo91000/leo-agent-manager/actions/runs/37221466077), timeline built from BuildKit step timestamps. Total 14.2 min: about 2.6 min setup, 7.9 min build, 3.7 min smoke tests.

| Phase | Seconds | Bound by |
| --- | ---: | --- |
| Checkout, `Prepare KVM build host` (mostly `rm -rf` of preinstalled SDKs; 47–149 s across runs), Docker restart | ~155 | disk |
| Cache manifest import, pulls of cached dependency/runtime layers | ~115 | network |
| `cairn` crate compile (`Finished release in 3m 57s`) | 238 | one CPU core |
| Guest disk: `mkfs.ext4 -d` of the 8 GB image plus `zstd -T2` | 49 | disk, two threads |
| Export and unpack into the runner's Docker | 41 | disk |
| Smoke tests: container 42 s, runner + nested KVM 121 s, retention 57 s, run serially | 220 | VM boots |

On main, the job also pushes the 3.39 GB image (33 layers; the largest is 1.68 GB). It exports the ~8 GB `mode=max` registry cache (81 blobs) and generates an SBOM. In [run 37138675072](https://github.com/leo91000/leo-agent-manager/actions/runs/37138675072) these took 97 s, 56 s and 40 s. That run also rebuilt the dependency layer (272 s) before the 281 s application compile. Docker notes that `mode=max` caches all intermediate layers, which raises import and export cost ([cache backends](https://docs.docker.com/build/cache/backends/)).

## Hard filters

- **Personal account.** Several products only register runners in an organization runner group.
- **Nested KVM at the depth the image job needs.** On a VM-based runner, the levels are:

  | Level | What runs there | Provided by |
  | --- | --- | --- |
  | L0 | Vendor host | Vendor |
  | L1 | Runner VM with `/dev/kvm` | Vendor; this is what vendors mean by "nested virtualization" |
  | L2 | Firecracker guest | This repository |
  | L3 | `KVM_RUN` probe inside the guest | Image job's smoke test |

  The image job asserts `nested=Y` on the runner, then executes `KVM_RUN` inside the guest.
  - GitHub's standard hosted runners demonstrably pass this ([run 36142279729](https://github.com/leo91000/leo-agent-manager/actions/runs/36142279729), [NESTED-KVM-RESEARCH.md](NESTED-KVM-RESEARCH.md)).
  - On bare metal the same work is only L1/L2. Linux enables `nested` by default since 4.20 ([kernel guide](https://www.kernel.org/doc/html/latest/virt/kvm/x86/running-nested-guests.html)).
  - **No VM-based vendor documents L3.** AWS describes only "your EC2 instance running a hypervisor (L1), and one or more virtual machines created within that instance (L2)" ([AWS guide](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/amazon-ec2-nested-virtualization.html)).
- **Android emulators need only `/dev/kvm` on the runner (L1).**

**Go/no-go probe for any option.** On a trial, run the unchanged `Prepare KVM build host` step and `node tests/runner-smoke.mjs` against an existing image digest. Require the `nested-kvm: … KVM_RUN` line. If it is missing, exclude the option for the image job.

## Comparison

**Price basis.**
- "Projected" assumes:

  | Jobs | Runner size | Avg duration (estimated) | Minutes / 30 d |
  | --- | --- | ---: | ---: |
  | `image` | 16 vCPU | 6.5 min | 6,325 |
  | `quality` | 16 vCPU | 3 min | 2,919 |
  | 4 browser jobs | 4 vCPU | 3.5 min | 10,105 |

- "Upper" uses today's durations: 20,992 min at 16 vCPU and 12,540 min at 4 vCPU.
- Android stays on free runners. Moving it would add about 8,900 min at today's durations.
- Namespace prices browsers on its 4×8 shape. Today's 16 GB of RAM (a 4×16 shape) bills 8 units per minute and would add ~$60.

| Option | Personal account | Nested KVM | Shapes (x64) | Persistent cache | Price / min (16 vCPU; 4 vCPU) | Est. monthly (projected / upper) | Public-repo notes | Migration |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **GitHub larger runners** | **No.** "only available for organizations and enterprises using the GitHub Team or GitHub Enterprise Cloud plans" ([docs](https://docs.github.com/en/actions/concepts/runners/larger-runners)) | `/dev/kvm` for Android documented ([hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [2023 changelog](https://github.blog/changelog/2023-02-23-hardware-accelerated-android-virtualization-on-actions-windows-and-linux-larger-hosted-runners/)); L3 unverified | 2–96 cores | None beyond Actions cache; custom images | $0.042; $0.012, whole-minute rounding, "not free for public repositories" ([pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing)) | ~$510 / ~$1,030 plus Team seats | Ephemeral hosted VMs | Org transfer plus Team plan; label change |
| **Blacksmith** | **No.** "limited to GitHub organizations and not available for personal repositories" ([quickstart](https://docs.blacksmith.sh/introduction/quickstart)) | x64: "Yes… KVM-dependent jobs" ([overview](https://docs.blacksmith.sh/blacksmith-runners/overview)); runners are Firecracker microVMs; L3 unverified | 2–32 vCPU, 4 GB/vCPU, "gaming CPUs" (model unverified) | Docker builder state and cache mounts on sticky disks, $0.50/GB-month, 7-day eviction ([Docker](https://docs.blacksmith.sh/blacksmith-caching/docker-builds), [sticky disks](https://docs.blacksmith.sh/blacksmith-caching/dependencies-sticky-disks)) | $0.032; $0.008, 3,000 free 2-vCPU min ([pricing](https://www.blacksmith.sh/pricing)) | ~$415 / ~$810 including ~100 GB cache | Branch protection limits sticky-disk commits to default-branch `push`/`schedule`/`workflow_dispatch` jobs | Org transfer; swap build actions |
| **Namespace** | **Yes.** "on either a personal account or an organization" ([docs](https://namespace.so/docs/solutions/github-actions)) | `linux/amd64` "Supported"; Android emulator "No additional KVM setup" ([nested virt](https://namespace.so/docs/architecture/compute/nestedvirt), [Android](https://namespace.so/docs/integrations/android-emulators)); L3 unverified | 2×4 to 32×64 plus custom ratios; "AMD EPYC" ([Linux](https://namespace.so/docs/architecture/compute/linux)) | NVMe cache volumes ([caching](https://namespace.so/docs/solutions/github-actions/caching)); in-runner "locally cached" builder recommended for 10 GB+ images ([Docker builds](https://namespace.so/docs/solutions/github-actions/docker-builds)) | $0.016; $0.004 prepaid. Overage $0.0015/unit-min. Team $100/month includes 100k unit-min and a 64-vCPU cap ([pricing](https://namespace.so/pricing)) | **~$265 / ~$485** (Team or Business plus ~$30 cache volumes) | Cache updates can be limited to chosen branches; a "restricted" access level exists for untrusted code ([access levels](https://namespace.so/docs/solutions/github-actions/runner-controls/access-levels)) | Labels; `container:` jobs need extra mounts ([containerized jobs](https://namespace.so/docs/solutions/github-actions/runner-controls/containerized-jobs)); `services:` undocumented |
| **Depot runners** | **No.** "only support repositories owned by GitHub organizations" ([overview](https://depot.dev/docs/github-actions/overview)) | **No.** "don't currently provide `/dev/kvm`" ([troubleshooting](https://depot.dev/docs/github-actions/troubleshooting)) | 2–64 vCPU, EPYC Genoa ([types](https://depot.dev/docs/github-actions/runner-types)) | Depot Cache, $0.20/GB-month | $0.048; $0.012 | n/a (fails KVM) | — | — |
| **Depot remote builds** from free hosted runners | Yes; OIDC trust takes "the GitHub user or organization name" ([integration](https://depot.dev/docs/container-builds/integrations/github-actions)) | Not needed: smoke tests stay on hosted runners | Builder "16 CPUs, 32 GB" ([overview](https://depot.dev/docs/container-builds/overview)) | Persistent NVMe BuildKit cache, 50 GB default | $0.04/build-min after 500 (Developer, $20/month) or 5,000 (Startup, $200/month) ([pricing](https://depot.dev/pricing)) | ~$136–200 / ~$310 (builds only) | Fork builds use isolated ephemeral builders without the main cache ([fork builds](https://depot.dev/blog/github-actions-oss-fork-builds)) | Swap in `depot/build-push-action`; `load: true` must copy 3.4 GB back. **Does not reach 5 min.** |
| **Depot CI** (separate CI) | Yes ([overview](https://depot.dev/docs/github-actions/overview)) | "enabled by default on every Depot CI sandbox" ([blog](https://depot.dev/blog/now-available-nested-virtualization-on-depot-ci)); L3 unverified | 2×8 to 64×256 | Depot Cache | $0.048; $0.012, per second ([CI overview](https://depot.dev/docs/ci/overview)) | ~$575 / — | Fork PR workflows "planned", not supported; GHCR with `GITHUB_TOKEN` "doesn't work" ([compatibility](https://depot.dev/docs/ci/compatibility)) | Copy workflows to `.depot/workflows`; PAT for GHCR |
| **Ubicloud** (premium) | Unverified; its code registers per-repository JIT runners | **Undocumented**; no KVM mention ([runner types](https://www.ubicloud.com/docs/github-actions-integration/runner-types)) | 2–30 vCPU, 4 GB/vCPU; premium on Ryzen 9 7950X3D | Transparent Actions cache, including Docker `type=gha`; 30 GB free per repository per week ([cache](https://www.ubicloud.com/docs/github-actions-integration/ubicloud-cache)); no persistent builder documented | $0.016; $0.004 ([pricing](https://www.ubicloud.com/docs/about/pricing)); standard runners closed to new customers ([notice](https://www.ubicloud.com/blog/ubicloud-price-adjustment-2026)) | ~$186 / ~$386 | Fresh JIT VM per job ([security](https://www.ubicloud.com/docs/github-actions-integration/security)) | Label change. Probe first. |
| **WarpBuild** | **No.** "an organization is a prerequisite" ([comparison](https://www.warpbuild.com/compare/github-actions)) | x64 label `nested-virtualization.enabled=true` exposes `/dev/kvm` ([docs](https://www.warpbuild.com/docs/ci/features/nested-virtualization)); L3 unverified | 2–32 vCPU ([runners](https://www.warpbuild.com/docs/ci/cloud-runners)) | Cache action, snapshots ($0.025/h each plus $0.04 per restore) ([pricing](https://www.warpbuild.com/pricing)) | $0.032; $0.008 | ~$380 / ~$770 plus cache | Org runner group must allow public repositories ([public repos](https://www.warpbuild.com/docs/ci/public-repos)) | Org transfer; labels |
| **RunsOn** (your AWS account) | Yes; "A GitHub organization (or personal account)" ([install](https://runs-on.com/guides/install)) | `nested-virt` label on Intel `c8i`/`m8i`/`r8i`, "x64 only" ([docs](https://runs-on.com/docs/runners/capabilities/nested-virtualization/), [v2.12.2](https://runs-on.com/changelog/v2.12.2/)); AWS lists more Intel families and documents only L2 ([AWS](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/amazon-ec2-nested-virtualization.html)) | Any EC2 type; c8i.4xlarge is 16 vCPU / 32 GiB | Sticky EBS disks with `sticky_cache: buildkit`; S3/ECR Docker cache ([sticky disks](https://runs-on.com/blog/introducing-sticky-disks/), [Docker cache](https://runs-on.com/docs/performance/caching/docker/)) | c8i.4xlarge on-demand $0.7497/h ≈ $0.0125/min in us-east-1, per second ([EC2](https://aws.amazon.com/ec2/pricing/on-demand/)); spot snapshot $0.3113/h ≈ $0.0052/min (volatile). License free for "personal non-commercial" projects, otherwise from €300/year ([pricing](https://runs-on.com/pricing/)) | **~$70–100 spot, ~$160–185 on-demand / ~$135–310** including EBS | Fresh EC2 instance per job; PR jobs "do not write back to the default-branch lineage"; public repos read `.github/runs-on.yml` only from the default branch ([labels](https://runs-on.com/docs/runners/labels/)) | CloudFormation stack (~15 min quickstart); long labels; AWS operations |
| **Self-hosted on OVHcloud** | Yes; repository runners need the repository owner ([add runners](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)) | Bare metal: the image job needs only standard L1/L2 nesting. Public Cloud instances: "your kernel may panic" after live migration ([FAQ](https://docs.ovhcloud.com/en/guides/public-cloud/cross-functional/faq-pci)) | RISE-L: Ryzen 9 9950X, 16c/32t, up to 5.7 GHz, 128 GB, 2×960 GB NVMe ([prices](https://www.ovhcloud.com/en-ie/bare-metal/prices/)) | Full control: local BuildKit state, cargo `target/` | Fixed €149.99/month plus equal setup fee; setup waived on 12/24-month terms per the [US FAQ](https://us.ovhcloud.com/bare-metal/faq/) (EU unverified); stock "Soon available" | **€150/month** (one run at a time; a second box at peaks) | GitHub: "should almost never be used for public repositories" ([secure use](https://docs.github.com/en/actions/reference/security/secure-use)) | Runner service, isolation tooling, patching |

**Excluded.**
- **Cirrus Runners** stopped accepting new customers on 2026-04-07 ([Cirrus Labs](https://cirruslabs.org/)).
- **actuated** requires "A GitHub organisation" and costs $150 per server per month on top of the server ([register](https://docs.actuated.com/register/), [pricing](https://actuated.com/pricing)).
- **BuildJet** reportedly stopped running jobs on 2026-03-31. This is unverified: the vendor page could not be fetched.
- An Intel OVH alternative, Advance-5 (Xeon 6527P), costs from €572.99–618.99/month depending on location.

## Speedup reasoning for the image job

**CPU-bound compile.** Cargo states that more codegen units let "more of a crate… be processed in parallel" ([profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)). With `codegen-units = 1` and thin LTO, extra vCPUs barely help the 238 s application compile. Only per-core speed helps.
- Estimate (unverified, no benchmark run): **~140–180 s** on Ryzen 9950X/7950X3D-class or recent EPYC/Xeon cores, versus GitHub's 4-vCPU runner.
- Dependency rebuilds (272 s when `Cargo.lock` changes) do parallelize, so 16 vCPUs help there.

**Network-bound cache.**
- A persistent local BuildKit cache removes ~115 s of cache import on PRs. Candidates: Namespace cache volume, Blacksmith sticky disk, RunsOn sticky disk, or a bare-metal host.
- It also removes the ~56 s `mode=max` registry cache export on main.
- The 97 s image push on main stays network-bound.
- Depot remote builds also remove the cache transfer, but then have to copy a 3.4 GB image back to the hosted runner for the KVM smoke tests.

**Disk-bound and serial steps.**
- With NVMe and no SDK cleanup, setup should drop to under 30 s.
- Guest disk creation and export should drop from 90 s to about 50 s.
- The smoke tests (220 s) are unchanged unless they run concurrently.

**Result.** Hardware plus a persistent cache gives an estimated **~6.5–7.5 min image job**, which sets the PR critical path at about 7–8 min. Reaching about 5 min also needs workflow changes:
1. A PR-only Rust profile override for the image build, such as more codegen units and no LTO. Main and tags keep the release profile. Estimated compile time is about 60 s on 16 vCPUs. The trade-off: PR images are not built with the release profile.
2. Concurrent smoke tests on a 16-vCPU/32–64 GB runner, cutting about 220 s to about 120 s.
3. A small artifact-build job that feeds the browser jobs, so they don't wait for clippy, tests and Python checks. Quality runs ~8 min and browser ~4.3 min today, and they are serial. With the change, the browser path would be about 1.5 min plus ~3 min.

These changes would also lower the free-runner floor. The quality path needs a persistent `target/` too: actions/cache restore alone takes 30–150 s today.

**Measured since, in PR #74 ([run 37241143537](https://github.com/leo91000/leo-agent-manager/actions/runs/37241143537)):**
- Change 3 is implemented. The build job takes 2m04s, and the build-plus-browser path takes 7.9 min.
- 16 codegen units alone cut the 238 s compile only to 222 s on the 4-vCPU runner, so the change was reverted. Change 1 therefore depends on disabling LTO or on faster cores, which is still unmeasured.
- Same-repository PR images are now published so a release can reuse them. That adds about 40 s of SBOM generation and 110 s of export and push to the PR image job, which a persistent local cache would not remove.

## Public-repository security

- **Enable "Require approval for all external contributors".** GitHub warns that with self-hosted runners, malicious code "will execute automatically" if approval is bypassed or granted ([repository Actions settings](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository)).
- **Route only same-repository events to paid or persistent runners.** For example: `runs-on: ${{ (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && '<paid label>' || 'ubuntu-latest' }}`. Fork PRs then keep today's free, isolated path and cannot spend paid minutes.
- **Let only `main` write persistent caches.** The current workflow already does this for the registry cache. Use the vendor setting: Namespace branch-restricted cache volumes, Blacksmith branch protection, RunsOn's PR fallback without write-back, or Ubicloud's default cache branch protection.
- **On a self-hosted OVH runner**, use single-job runners only:
  - registered with `--ephemeral` or a JIT configuration ([runner reference](https://docs.github.com/en/actions/reference/runners/self-hosted-runners), [JIT API](https://docs.github.com/en/rest/actions/self-hosted-runners));
  - each in a freshly created VM, because a long-lived Docker cache on the host could be poisoned;
  - no production credentials on the box.
- **Never use the Intel production host.** DEPLOYMENT.md states that "No persistent GitHub runner or GitHub credential is installed on the Intel host" ([DEPLOYMENT.md](DEPLOYMENT.md#local-intel-qualification)).
- **Main-branch image jobs hold `packages: write`.** Any runner they use is part of the release supply chain.

## Recommendation

**Primary: Namespace.** Its trial covers the go/no-go probe before any spend.
1. Move `image`, `quality` and the browser jobs for main and same-repository PRs; keep fork PRs and `resolve` on `ubuntu-latest`.
2. Use a cache volume for BuildKit and `target/`, writable only from `main`.
3. Apply the three workflow changes above.
4. Revalidate the PR-only `systemctl restart docker` step and the `container:` + `services:` browser jobs on its images.

**Cost: ~$265/month** projected at the measured volume, up to ~$485/month if durations do not fall. The Team plan is required: one run peaks at 32 vCPUs and overlapping PR and main runs reach 64.

**Cheaper fallbacks, depending on the probe:**
- **RunsOn with `c8i` spot, ~$70–100/month** (or €300/year more if the project is commercial). Choose it if Namespace fails L3 and Intel L3 passes on EC2. It takes AWS administration, and AWS recommends bare metal for performance-sensitive nested workloads.
- **OVH RISE-L, €150/month.** Choose it only if both managed probes fail. It needs ephemeral per-job VM orchestration.

**Blocker check:** none of the recommended paths needs an organization. Blacksmith, WarpBuild, Depot runners and GitHub larger runners would need the repository transferred to an organization (and GitHub's Team plan for larger runners). That transfer would change the GHCR image path used by Coolify.

## Reproduce

```sh
for wf in ci.yaml android.yaml; do
  gh api --paginate "repos/leo91000/leo-agent-manager/actions/workflows/$wf/runs?created=>=2026-09-20&per_page=100" \
    --jq '.workflow_runs[] | [.id, .event, .head_branch, .conclusion, .created_at] | @tsv'
done > runs.tsv
cut -f1 runs.tsv | xargs -P 8 -I{} sh -c \
  'gh api --paginate "repos/leo91000/leo-agent-manager/actions/runs/{}/jobs?filter=all&per_page=100" \
     --jq ".jobs[] | {name, conclusion, started_at, completed_at}" > jobs-{}.json'
# Sum ceil((completed_at - started_at) / 60) per job name, skipping skipped jobs; scale by 30 / window days.
node scripts/ci-timings.mjs 37221466077 37138675072   # per-step durations of the runs analysed above
```
