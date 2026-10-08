package dev.leo.manager.data

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.*
import okhttp3.Interceptor
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody

/** Owns the current installation route; account operations and binaries stay on HTTPS. */
class DirectTransport(private val installationId: String?) : Interceptor {
    private val current = AtomicReference<DirectChannel?>()
    private val observed = MutableStateFlow("relay")
    val route = observed.asStateFlow()

    fun attach(channel: DirectChannel) {
        current.getAndSet(channel)?.close()
    }

    fun close() {
        current.getAndSet(null)?.close()
        observed.value = "relay"
    }

    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        val prefix = "/api/installations/${installationId?.let(::segment)}/api/"
        val acceptsApplication =
            request.header("Accept") in setOf("application/json", "text/event-stream")
        val channel =
            current.get().takeIf {
                installationId != null &&
                    request.url.encodedPath.startsWith(prefix) &&
                    acceptsApplication
            }
        if (channel != null) {
            try {
                val response = channel.request(request, prefix, chain.call()::isCanceled)
                observed.value = "direct"
                return response
            } catch (error: IOException) {
                if (current.compareAndSet(channel, null)) channel.close()
                observed.value = "relay"
                if (chain.call().isCanceled() || request.method !in setOf("GET", "HEAD"))
                    throw error
            }
        }
        val response = chain.proceed(request)
        if (request.url.encodedPath.startsWith(prefix) && acceptsApplication)
            observed.value = "relay"
        return response
    }
}

/** Adapter for the existing v4 binary envelope; native ownership stays with its caller. */
class DirectChannel(
    private val send: (ByteArray) -> Unit,
    private val dispose: () -> Unit,
) {
    companion object {
        // relay-protocol/src/{lib,data_channel}.rs; v4 shares the relay's limits.
        const val MAX_BODY = 8_000_000
        const val MAX_FRAME = ((MAX_BODY + 2) / 3) * 4 + 65_536
        const val MAX_PACKET = 16_384
        const val HEADER = 13
    }

    private val pending = ConcurrentHashMap<String, CompletableFuture<JsonObject>>()
    private val closed = AtomicBoolean()
    private val transfers = AtomicInteger()

    private class Assembly(val total: Int) {
        val bytes = ByteArrayOutputStream()
        val started = System.nanoTime()
    }

    private val assemblies = mutableMapOf<Int, Assembly>()
    private var buffered = 0

    fun close() {
        if (!closed.compareAndSet(false, true)) return
        pending.values.forEach {
            it.completeExceptionally(IOException("Connexion directe interrompue"))
        }
        pending.clear()
        synchronized(assemblies) {
            assemblies.clear()
            buffered = 0
        }
        dispose()
    }

    fun receive(packet: ByteArray) {
        val frame =
            synchronized(assemblies) {
                val envelope = ByteBuffer.wrap(packet)
                require(packet.size in HEADER..MAX_PACKET && envelope.get() == 1.toByte())
                require(
                    assemblies.values.none {
                        System.nanoTime() - it.started >= TimeUnit.SECONDS.toNanos(30)
                    }
                )
                val id = envelope.int
                val total = envelope.int
                val offset = envelope.int
                val length = packet.size - HEADER
                require(id != 0 && total in 0..MAX_FRAME && offset in 0..total)
                if (total == 0 && offset == 0 && length == 0) {
                    assemblies.remove(id)?.let { buffered -= it.bytes.size() }
                    return
                }
                require(length > 0 && length <= total - offset && buffered + length <= MAX_FRAME)
                if (offset == 0) {
                    require(id !in assemblies && assemblies.size < 32)
                    assemblies[id] = Assembly(total)
                }
                val assembly = requireNotNull(assemblies[id])
                require(assembly.total == total && assembly.bytes.size() == offset)
                assembly.bytes.write(packet, HEADER, length)
                buffered += length
                if (assembly.bytes.size() != total) return
                assemblies.remove(id)
                buffered -= total
                wireJson
                    .parseToJsonElement(String(assembly.bytes.toByteArray(), Charsets.UTF_8))
                    .jsonObject
            }
        require(frame["type"]?.jsonPrimitive?.content == "response")
        pending[frame["id"]?.jsonPrimitive?.content]?.complete(frame)
    }

    fun request(request: Request, prefix: String, canceled: () -> Boolean): Response {
        if (canceled()) throw IOException("Requête annulée")
        if (closed.get()) throw IOException("Connexion directe interrompue")
        val id = java.util.UUID.randomUUID().toString()
        val result = CompletableFuture<JsonObject>()
        pending[id] = result
        val path =
            "/api/" +
                request.url.encodedPath.removePrefix(prefix) +
                (request.url.encodedQuery?.let { "?$it" } ?: "")
        val frame = buildJsonObject {
            put("type", "request")
            put("id", id)
            put("account_id", "")
            put("role", "member")
            put("method", request.method)
            put("path", path)
            put(
                "headers",
                buildJsonArray {
                    add(
                        buildJsonArray {
                            add("accept")
                            add(request.header("Accept").orEmpty())
                        }
                    )
                },
            )
            put("body", "")
        }
            .toString()
            .toByteArray(Charsets.UTF_8)
        // Same 13-byte network-order envelope as relay-protocol/src/data_channel.rs.
        try {
            require(frame.size <= MAX_FRAME)
            val transfer = transfers.updateAndGet { if (it == -1) 1 else it + 1 }
            var offset = 0
            while (offset < frame.size) {
                if (canceled()) throw IOException("Requête annulée")
                val length = minOf(MAX_PACKET - HEADER, frame.size - offset)
                send(
                    ByteBuffer.allocate(HEADER + length)
                        .put(1)
                        .putInt(transfer)
                        .putInt(frame.size)
                        .putInt(offset)
                        .put(frame, offset, length)
                        .array()
                )
                offset += length
            }
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(30)
            while (true) {
                if (canceled()) throw IOException("Requête annulée")
                if (System.nanoTime() >= deadline) throw IOException("Connexion directe expirée")
                val response =
                    try {
                        result.get(50, TimeUnit.MILLISECONDS)
                    } catch (_: TimeoutException) {
                        continue
                    }
                val bytes = Base64.getDecoder().decode(response["body"]!!.jsonPrimitive.content)
                require(bytes.size <= MAX_BODY)
                val builder =
                    Response.Builder()
                        .request(request)
                        .protocol(Protocol.HTTP_1_1)
                        .code(response["status"]!!.jsonPrimitive.int)
                        .message("Direct")
                response["headers"]!!.jsonArray.forEach {
                    val pair = it.jsonArray
                    builder.addHeader(pair[0].jsonPrimitive.content, pair[1].jsonPrimitive.content)
                }
                return builder.body(bytes.toResponseBody()).build()
            }
        } finally {
            pending.remove(id)
        }
    }
}
