# Android plugin setup draft: saved-state and local-file reference

Measured on 2026-10-19 with frozen tree
`a79f3ddd189b31f78ee8aea282a5b3a9fcce5790`. The full Android JVM suite passes
518/518 tests, including 40 plugin journeys, 4 store contracts and 9 scope transitions.
Lint/APKs/device qualification are separate checks.

The valid setup workload contains 20 ordinary fields with 10,000 characters each:
six quote defaults and14 ASCII fields. The shared regression saves the actual
Compose registry into an Android Parcel. Its reference failed at 1,543,572 bytes;
the corrected saved state is 1,280 bytes. Ordinary edits survive recreation,
while typed secrets are cleared. The corresponding canonical JSON file contains
260,251 encoded bytes. These are different representations; no compression ratio
or end-to-end speedup is claimed.

The store harness performs one atomic write, one read through a new store object,
then five repeated reads, checking identical ordinary values each time.

| Measurement | Local result |
| --- | ---: |
| Encoded file |260,251 B|
| Atomic write wall time |36.992 ms|
| First new-store read |40.727 ms|
| Mean repeated read,5 reads |12.477 ms|
| Current-thread CPU for write/read sequence |139.683 ms|

Conditions: Linux 6.12.109 x86_64, glibc 2.36, 7 virtual CPUs, 19,929,366,528 bytes
RAM, nested agent VM. Robolectric's application files are on the host's tmpfs;
the OS cache is retained. The first read is **not a physical cold-disk read**.
The full JVM suite was active; no other heavy test job was running during this
measurement. The values do not qualify flash storage, a phone or production
network latency and are descriptive single-run evidence, not timing assertions.

Reproduce from `apps/android/` with pinned mise/JDK/Gradle:

```sh
mise exec -- ./gradlew testDebugUnitTest \
  --tests '*PluginSettingsDraftStoreTest' \
  --tests '*PluginsJourneyTest.largeSetupSavesOnlySmallEditorReferencesAndOrdinaryEdits' \
  --max-workers=1 --no-daemon
```

`PluginSettingsDraftStoreTest.largeOrdinaryInputsUseBoundedLocalStorage` prints
encoded bytes, write/read nanoseconds and thread CPU. The shared case prints the
Parcel size. Other contracts cover declared-field filtering, scope isolation,
AtomicFile recovery, corrupt/expired files, retention and retirement of open
handles after session cleanup. No secret values appear in this measurement.


## Unsaved resource editor follow-up

The same saved-state boundary also applies before resource submission. A small
valid schema accepts 29 ordinary fields of up to 10,000 characters each plus a
secret. Typing 10,000 quotation marks into each ordinary field produced an actual
1,173,992-byte saved Parcel before the correction. The corrected shared JVM case
uses 13,200 bytes, restores the ordinary fields and clears the typed credential.
No resource is submitted: this unsaved draft exceeds the server's aggregate save
limit even though each input fits its field limit. The result qualifies
Robolectric saved-state behavior, not physical-device storage.

The existing store harness now runs two workloads through the same measurement
sequence: the 20-field setup reference above, then the 29-field resource input.
It prints each workload's encoded bytes, atomic-write latency, new-store read,
five-read mean and current-thread CPU. The resource workload has no enlarged
manifest defaults, and snapshots contain only declared ordinary keys. The setup
table above remains the historical pre-merge measurement; it is not a measurement
of the new resource workload. Fresh full-check output records that workload
separately, with OS cache retained and no physical cold-read claim.

Include the resource recreation case when reproducing the follow-up:

```sh
mise exec -- ./gradlew testDebugUnitTest \
  --tests '*PluginSettingsDraftStoreTest' \
  --tests '*PluginsJourneyTest.largeUnsavedResourceInputsUseSmallSavedStateAndNeverPersistSecrets' \
  --max-workers=1 --no-daemon
```


The current two-workload harness output records the following separate local
trial. Full Gradle qualification is still pending. Native integration tests and
other build/test jobs overlapped this run on the same nested VM; the OS cache was
retained. These are descriptive results, with no physical cold read, device or
network qualification, timing assertion or cross-run speedup claim.

| Measurement | Setup reference | Unsaved resource |
| --- | ---: | ---: |
| Encoded file |260,251 B|580,368 B|
| Atomic write wall time |2.696 ms|18.106 ms|
| First new-store read |4.899 ms|19.164 ms|
| Mean repeated read,5 reads |4.660 ms|15.142 ms|
| Current-thread CPU for write/read sequence |30.762 ms|100.166 ms|

The exact nanosecond values and workload descriptions are retained in the JSON
reference beside this note. The setup file representation is unchanged; the new
resource workload has 29 quote-only fields, so its encoded size differs.
