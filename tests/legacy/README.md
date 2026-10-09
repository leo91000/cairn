# Migration oracle

This is the frozen v0.11 Node backend, retained only for compatibility tests, fixture seeding and comparative benchmarks. It is never packaged, started by browser tests, or used by the application. New backend behavior belongs in `crates/installation/` with native integration tests. Remove this oracle when migration coverage no longer needs the old implementation.
