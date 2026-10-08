package dev.leo.manager.data

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.*
import okhttp3.Call
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import okio.BufferedSource
import okio.ForwardingSink
import okio.buffer
import okio.source

/** Owns the current installation route; account operations and binaries stay on HTTPS. */
class DirectTransport(private val installationId: String?, private val switched: () -> Unit = {}) :
    Interceptor {
    private class Selection(val channel: DirectChannel?)

    private val prefix = "/api/installations/${installationId?.let(::segment)}/api/"
    private val current = AtomicReference<DirectChannel?>()
    private val observed = MutableStateFlow("relay")
    val route = observed.asStateFlow()

    fun attach(channel: DirectChannel) {
        channel.onClosed = {
            if (current.compareAndSet(channel, null)) {
                observed.value = "relay"
                switched()
            }
        }
        current.getAndSet(channel)?.close()
        switched()
    }

    fun close() {
        current.getAndSet(null)?.close()
        observed.value = "relay"
    }

    private fun channelFor(request: Request): DirectChannel? =
        current.get().takeIf {
            installationId != null &&
                request.url.encodedPath.startsWith(prefix) &&
                request.header("Accept") in setOf("application/json", "text/event-stream")
        }

    internal fun newCall(
        client: OkHttpClient,
        request: Request,
        forceRelay: Boolean = false,
    ): Call {
        val channel = if (forceRelay) null else channelFor(request)
        // Bind routing before enqueue so the deadline and the selected peer cannot disagree.
        val prepared = request.newBuilder().tag(Selection::class.java, Selection(channel)).build()
        return client.newCall(prepared).also { call ->
            if (channel != null && client.callTimeoutMillis == 30000)
                call.timeout().timeout(65, TimeUnit.SECONDS)
        }
    }

    internal fun wasDirect(response: Response) =
        response.request.tag(DirectChannel::class.java) != null

    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        val acceptsApplication =
            request.header("Accept") in setOf("application/json", "text/event-stream")
        val selection = request.tag(Selection::class.java)
        val channel = if (selection != null) selection.channel else channelFor(request)
        val clientMessage = hasClientMessageId(request, prefix)
        var replayedMessage = false
        if (channel != null) {
            try {
                val response = channel.request(request, prefix, chain.call()::isCanceled)
                if (current.get() === channel) observed.value = "direct"
                return response
            } catch (error: IOException) {
                if (error is DirectLost) channel.close()
                val transportFailure =
                    error is DirectLost || error is DirectNotSent || error is DirectRequestTimeout
                if (
                    !transportFailure ||
                        chain.call().isCanceled() ||
                        (error !is DirectNotSent &&
                            request.method !in setOf("GET", "HEAD") &&
                            !clientMessage)
                )
                    throw error
                replayedMessage = clientMessage && error !is DirectNotSent
            }
        }
        val response = chain.proceed(request)
        if (request.url.encodedPath.startsWith(prefix) && acceptsApplication)
            observed.value = "relay"
        if (replayedMessage && response.code == 409) {
            val error = runCatching {
                wireJson
                    .parseToJsonElement(response.peekBody(DirectChannel.MAX_BODY.toLong()).string())
                    .jsonObject["error"]
                    ?.jsonPrimitive
                    ?.content
            }
                .getOrNull()
            if (error == "This message identifier has already been used.") {
                response.close()
                return response
                    .newBuilder()
                    .code(200)
                    .message("OK")
                    .removeHeader("Content-Length")
                    .header("Content-Type", "application/json")
                    .body("{}".toResponseBody("application/json".toMediaType()))
                    .build()
            }
        }
        return response
    }

    private fun hasClientMessageId(request: Request, prefix: String): Boolean {
        if (
            request.method != "POST" ||
                !request.url.encodedPath.startsWith(prefix) ||
                !Regex("chats/[^/]+/messages").matches(request.url.encodedPath.removePrefix(prefix))
        )
            return false
        val body = request.body ?: return false
        if (
            body.isOneShot() ||
                body.isDuplex() ||
                body.contentLength() !in 0..DirectChannel.MAX_BODY.toLong()
        )
            return false
        return runCatching {
                val buffer = Buffer()
                body.writeTo(buffer)
                val id =
                    wireJson.parseToJsonElement(buffer.readUtf8()).jsonObject["id"]?.jsonPrimitive
                id?.isString == true && id.content.isNotBlank()
            }
            .getOrDefault(false)
    }
}

internal class DirectLost(message: String, cause: Throwable? = null) : IOException(message, cause)

internal class DirectNotSent(message: String) : IOException(message)

internal class DirectRequestTimeout : IOException("Délai de réponse directe dépassé")

internal class DirectStreamLost(cause: IOException) : IOException(cause.message, cause)

/** Adapter for the existing v4 binary envelope; native ownership stays with its caller. */
class DirectChannel(
    private val send: (ByteArray, () -> Boolean, Long) -> Unit,
    private val dispose: () -> Unit,
    private val valid: () -> Boolean = { true },
) {
    companion object {
        // relay-protocol/src/{lib,data_channel}.rs; v4 shares the relay's limits.
        const val MAX_BODY = 8_000_000
        const val MAX_FRAME = ((MAX_BODY + 2) / 3) * 4 + 65_536
        const val MAX_PACKET = 16_384
        const val HEADER = 13
    }

    private class Exchange {
        val frames = ArrayBlockingQueue<JsonObject>(2)
        @Volatile var failure: IOException? = null
        @Volatile var stream = false
    }

    private val pending = ConcurrentHashMap<String, Exchange>()
    internal var onClosed: () -> Unit = {}
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
            it.failure = DirectLost("Connexion directe interrompue")
        }
        pending.clear()
        synchronized(assemblies) {
            assemblies.clear()
            buffered = 0
        }
        onClosed()
        dispose()
    }

    fun receive(packet: ByteArray) {
        if (!valid()) {
            close()
            return
        }
        if (closed.get()) return
        try {
            receiveFrame(packet)
        } catch (_: Exception) {
            // Invalid wire data is a transport failure; callers resume at their accepted cursor.
            close()
        }
    }

    fun checkDeadline() {
        val expired =
            synchronized(assemblies) {
                assemblies.values.any {
                    System.nanoTime() - it.started >= TimeUnit.SECONDS.toNanos(30)
                }
            }
        if (expired) close()
    }

    private fun receiveFrame(packet: ByteArray) {
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
        val type = frame["type"]!!.jsonPrimitive.content
        require(type in setOf("response", "stream_start", "stream_chunk", "stream_end"))
        require(frame["type"]!!.jsonPrimitive.isString)
        val id = frame["id"]!!.jsonPrimitive
        require(id.isString)

        // Validate the whole v4 response before exposing it to the HTTP/live reader.
        // Decoder failures then follow the same connection-loss recovery as bad envelopes.
        when (type) {
            "response",
            "stream_start" -> {
                decodeBody(frame, MAX_BODY)
                val status = frame["status"]!!.jsonPrimitive
                require(!status.isString && status.int in 100..999)
                val headers = okhttp3.Headers.Builder()
                frame["headers"]!!.jsonArray.forEach {
                    val pair = it.jsonArray
                    require(pair.size == 2 && pair.all { value -> value.jsonPrimitive.isString })
                    headers.add(pair[0].jsonPrimitive.content, pair[1].jsonPrimitive.content)
                }
            }
            "stream_chunk" -> decodeBody(frame, 65536)
            "stream_end" -> {
                val failed = frame["failed"]!!.jsonPrimitive
                require(!failed.isString && failed.booleanOrNull != null)
            }
        }

        val exchange = pending[id.content] ?: return
        if (type == "response") require(!exchange.stream)
        if (type == "stream_start") {
            synchronized(pending) {
                require(!exchange.stream && pending.values.count { it.stream } < 8)
                exchange.stream = true
            }
        }
        if (type in setOf("stream_chunk", "stream_end")) require(exchange.stream)
        require(exchange.frames.offer(frame)) { "Flux direct sans crédit" }
    }

    fun request(request: Request, prefix: String, canceled: () -> Boolean): Response {
        if (canceled()) throw IOException("Requête annulée")
        if (closed.get()) throw DirectNotSent("Connexion directe interrompue")
        val id = java.util.UUID.randomUUID().toString()
        val payload = Buffer()
        val bounded =
            object : ForwardingSink(payload) {
                    override fun write(source: Buffer, byteCount: Long) {
                        if (payload.size + byteCount > MAX_BODY)
                            throw IOException("Requête trop volumineuse")
                        super.write(source, byteCount)
                    }
                }
                .buffer()
        request.body?.writeTo(bounded)
        bounded.flush()
        val contentType = request.header("Content-Type") ?: request.body?.contentType()?.toString()
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
                    for (name in
                        listOf(
                            "accept",
                            "range",
                            "if-none-match",
                            "if-modified-since",
                            "last-event-id",
                            "mcp-protocol-version",
                            "mcp-method",
                        )) {
                        request.headers.values(name).forEach { value ->
                            add(
                                buildJsonArray {
                                    add(name)
                                    add(value)
                                }
                            )
                        }
                    }
                    contentType?.let {
                        add(
                            buildJsonArray {
                                add("content-type")
                                add(it)
                            }
                        )
                    }
                },
            )
            put("body", Base64.getEncoder().encodeToString(payload.readByteArray()))
        }
            .toString()
            .toByteArray(Charsets.UTF_8)
        val exchange = Exchange()
        synchronized(pending) {
            if (closed.get()) throw DirectNotSent("Connexion directe interrompue")
            if (pending.size >= 32) throw DirectNotSent("Trop de requêtes directes simultanées")
            pending[id] = exchange
        }
        var streaming = false
        try {
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(30)
            emit(frame, canceled, deadline)
            val response = await(exchange, canceled, deadline)
            val bytes = decodeBody(response, MAX_BODY)
            val builder =
                Response.Builder()
                    .request(request.newBuilder().tag(DirectChannel::class.java, this).build())
                    .protocol(Protocol.HTTP_1_1)
                    .code(response["status"]!!.jsonPrimitive.int)
                    .message("Direct")
            response["headers"]!!.jsonArray.forEach {
                val pair = it.jsonArray
                builder.addHeader(pair[0].jsonPrimitive.content, pair[1].jsonPrimitive.content)
            }
            if (response["type"]!!.jsonPrimitive.content == "stream_start") {
                streaming = true
                val input =
                    object : InputStream() {
                        private var chunk = bytes
                        private var offset = 0
                        private var ended = false
                        private var needsCredit = true

                        override fun read(): Int {
                            val byte = ByteArray(1)
                            return if (read(byte, 0, 1) < 0) -1 else byte[0].toInt() and 255
                        }

                        override fun read(target: ByteArray, start: Int, length: Int): Int {
                            if (length == 0) return 0
                            if (!valid()) this@DirectChannel.close()
                            while (offset == chunk.size) {
                                if (ended) return -1
                                if (needsCredit) {
                                    control("stream_credit", id)
                                    needsCredit = false
                                }
                                val next =
                                    await(
                                        exchange,
                                        canceled,
                                        System.nanoTime() + TimeUnit.SECONDS.toNanos(45),
                                    )
                                when (next["type"]!!.jsonPrimitive.content) {
                                    "stream_chunk" -> {
                                        chunk = decodeBody(next, 65536)
                                        offset = 0
                                        needsCredit = true
                                    }
                                    "stream_end" -> {
                                        ended = true
                                        pending.remove(id)
                                        if (next["failed"]!!.jsonPrimitive.boolean)
                                            throw IOException("Flux direct interrompu")
                                        return -1
                                    }
                                    else -> throw IOException("Trame de flux inattendue")
                                }
                            }
                            if (canceled()) throw IOException("Requête annulée")
                            exchange.failure?.let { throw it }
                            val count = minOf(length, chunk.size - offset)
                            chunk.copyInto(target, start, offset, offset + count)
                            offset += count
                            return count
                        }

                        override fun close() {
                            ended = true
                            pending.remove(id)
                            runCatching { control("cancel", id) }
                        }
                    }
                val source = input.source().buffer()
                return builder
                    .body(
                        object : ResponseBody() {
                            override fun contentType() =
                                response["headers"]!!
                                    .jsonArray
                                    .firstOrNull {
                                        it.jsonArray[0]
                                            .jsonPrimitive
                                            .content
                                            .equals("content-type", true)
                                    }
                                    ?.jsonArray
                                    ?.get(1)
                                    ?.jsonPrimitive
                                    ?.content
                                    ?.toMediaType()

                            override fun contentLength() = -1L

                            override fun source(): BufferedSource = source
                        }
                    )
                    .build()
            }
            return builder.body(bytes.toResponseBody()).build()
        } catch (error: IOException) {
            runCatching { control("cancel", id) }
            throw error
        } finally {
            if (!streaming) pending.remove(id)
        }
    }

    private fun decodeBody(frame: JsonObject, limit: Int): ByteArray {
        val body = frame["body"]!!.jsonPrimitive
        require(body.isString)
        val encoded = body.content
        require(encoded.length <= ((limit + 2) / 3) * 4)
        return Base64.getDecoder().decode(encoded).also {
            require(it.size <= limit)
            require(Base64.getEncoder().encodeToString(it) == encoded)
        }
    }

    private fun await(exchange: Exchange, canceled: () -> Boolean, deadline: Long): JsonObject {
        while (true) {
            if (canceled()) throw IOException("Requête annulée")
            if (!valid()) close()
            exchange.failure?.let { throw it }
            if (System.nanoTime() >= deadline) {
                throw DirectRequestTimeout()
            }
            exchange.frames.poll(50, TimeUnit.MILLISECONDS)?.let {
                return it
            }
        }
    }

    private fun control(type: String, id: String) {
        emit(
            buildJsonObject {
                put("type", type)
                put("id", id)
            }
                .toString()
                .toByteArray(),
            deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(100),
        )
    }

    private fun emit(
        frame: ByteArray,
        canceled: () -> Boolean = { false },
        deadline: Long,
    ) {
        require(frame.size <= MAX_FRAME)
        val transfer = transfers.updateAndGet { if (it == -1) 1 else it + 1 }
        var offset = 0
        try {
            while (offset < frame.size) {
                if (canceled()) throw IOException("Requête annulée")
                if (!valid()) close()
                if (closed.get()) {
                    if (offset == 0) throw DirectNotSent("Connexion directe interrompue")
                    throw DirectLost("Connexion directe interrompue")
                }
                val length = minOf(MAX_PACKET - HEADER, frame.size - offset)
                val packet =
                    ByteBuffer.allocate(HEADER + length)
                        .put(1)
                        .putInt(transfer)
                        .putInt(frame.size)
                        .putInt(offset)
                        .put(frame, offset, length)
                        .array()
                try {
                    send(packet, canceled, deadline)
                } catch (error: IOException) {
                    if (canceled()) throw error
                    if (offset == 0 && error is DirectRequestTimeout)
                        throw DirectNotSent(error.message!!)
                    if (error is DirectRequestTimeout || (offset == 0 && error is DirectNotSent))
                        throw error
                    throw DirectLost("Connexion directe interrompue", error)
                }
                offset += length
            }
        } catch (error: IOException) {
            if (offset in 1 until frame.size)
                runCatching {
                    send(
                        ByteBuffer.allocate(HEADER)
                            .put(1)
                            .putInt(transfer)
                            .putInt(0)
                            .putInt(0)
                            .array(),
                        { false },
                        System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(100),
                    )
                }
            if (error is DirectLost) close()
            throw error
        }
    }
}
