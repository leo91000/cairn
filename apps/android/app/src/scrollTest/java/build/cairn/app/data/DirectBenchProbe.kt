package build.cairn.app.data

import kotlinx.coroutines.delay

/** Poll an application response; a ready peer alone does not prove the direct route works. */
internal suspend fun awaitDirectBenchRead(api: CairnApi, read: suspend () -> Unit) {
    do {
        // Reads share the real account's 300/minute budget on both transports. Leave room
        // for the 64 in-flight lifecycle reads and other clients while setup is pending.
        delay(500)
        read()
    } while (api.transport.route.value != "direct")
}
