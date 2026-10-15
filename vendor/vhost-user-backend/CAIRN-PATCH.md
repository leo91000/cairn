# Cairn's pinned vhost-user exit-event ownership fix

Source: the crates.io `vhost-user-backend` **0.23.0** archive, SHA-256
`555753b65bc33837bd011f981e2c6d52d8ddc330d92c2deb9aa3f0b378a58bcc`,
the checksum recorded in Cairn's Cargo.lock before this patch. Upstream revision:
[`4cc2d89ce4897fd90c4bbaf920a5f89a4420547c`](https://github.com/rust-vmm/vhost/tree/4cc2d89ce4897fd90c4bbaf920a5f89a4420547c/vhost-user-backend).
`LICENSE-APACHE` is upstream's root `LICENSE` at that revision.

Compared with the archive, this directory:

- patches only `src/event_loop.rs`;
- adds `LICENSE-APACHE` and this file;
- omits `.cargo_vcs_info.json` and the dependency's unused `Cargo.lock`.

Every other file, including `Cargo.toml`, is byte-identical. To verify:

```sh
curl -sSfLO https://static.crates.io/crates/vhost-user-backend/vhost-user-backend-0.23.0.crate
sha256sum vhost-user-backend-0.23.0.crate
tar xzf vhost-user-backend-0.23.0.crate
diff -rq vhost-user-backend-0.23.0 vendor/vhost-user-backend
```

`VringEpollHandler` previously registered the shutdown consumer with
`into_raw_fd()` and kept only its notifier. Epoll borrows a descriptor; it does
not close it. Every backend teardown therefore leaked one eventfd, eventually
exhausting the runner's descriptor limit and preventing VM startup and recovery.

The handler now retains both owned handles and registers the consumer with
`as_raw_fd()`. Dropping the handler releases both handles after its worker has
stopped; a failed registration releases them on the error path as well. Shutdown
notification and queue handling are unchanged. This does not change stored bytes,
read/write processing, durability barriers, or S3 requests.

The regression runs the real Cairn block backend through 64 startup/teardown
cycles in a child process, alternating explicit close and drop. The descriptor
count must return to its starting value. It also checks socket removal and
completes without a frontend connection, exercising shutdown of blocked workers.
Locally, the unpatched dependency increased the count from 4 to 68; the patched
dependency kept it at 4. These are process-local resource measurements, not
production storage latency measurements.

```sh
pnpm test:backend -p cairn-installation --lib \
  storage::vhost::tests::repeated_backend_teardown_releases_file_descriptors -- --exact --nocapture
```

Remove this patch when an approved pinned upstream release retains the exit-event
consumer, preserving the lifecycle regression.
