package dev.leo.manager.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import java.io.IOException
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.selects.select
import kotlinx.serialization.json.*
import okhttp3.Call
import org.webrtc.*

/** One authorized installation peer. Lifecycle and network changes never stop agent runs. */
internal class NativeDirect(private val api: LeoApi, context: Context) {
    companion object {
        private val initialization = Any()
        private var initialized = false
    }

    private val stopped = AtomicBoolean()
    private val application = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val manager = application.getSystemService(ConnectivityManager::class.java)
    private val active = AtomicReference<Attempt?>()
    private val changed = Channel<Unit>(Channel.CONFLATED)
    @Volatile private var network = manager.activeNetwork
    private val restart = AtomicBoolean()
    private val callback =
        object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(next: Network) {
                if (next != network) networkChanged(next)
            }

            override fun onLost(previous: Network) {
                if (previous == network) networkChanged(null)
            }
        }

    private fun networkChanged(next: Network?) {
        network = next
        restart.set(true)
        active.get()?.fail()
        changed.trySend(Unit)
    }

    init {
        manager.registerDefaultNetworkCallback(callback)
        scope.launch {
            try {
                synchronized(initialization) {
                    if (!initialized) {
                        PeerConnectionFactory.initialize(
                            PeerConnectionFactory.InitializationOptions.builder(application)
                                .setInjectableLogger({ _, _, _ -> }, Logging.Severity.LS_NONE)
                                .createInitializationOptions()
                        )
                        initialized = true
                    }
                }
            } catch (_: LinkageError) {
                // An unsupported native ABI keeps the fully functional relay.
                return@launch
            }
            while (isActive) {
                val attempt = Attempt()
                active.set(attempt)
                try {
                    attempt.connect(restart.getAndSet(false))
                    attempt.failed.await()
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    // Direct is optional. Malformed/unsupported metadata closes this peer.
                    // The normal relay reports installation/session failures.
                } finally {
                    active.compareAndSet(attempt, null)
                    withContext(NonCancellable) { attempt.close() }
                }
                withTimeoutOrNull(15000) { changed.receive() }
            }
        }
    }

    fun close() {
        if (!stopped.compareAndSet(false, true)) return
        manager.unregisterNetworkCallback(callback)
        active.get()?.fail()
        scope.cancel()
    }

    private inner class Attempt {
        val failed = CompletableDeferred<Unit>()
        private val opened = CompletableDeferred<Unit>()
        private val expires = AtomicLong()
        private val candidates = Channel<JsonObject>(16)
        private val packets = Channel<ByteArray>(32)
        private val jobs = mutableListOf<Job>()
        private val events = AtomicReference<Call?>()
        private var factory: PeerConnectionFactory? = null
        private var peer: PeerConnection? = null
        private var data: DataChannel? = null
        private var traffic: DirectChannel? = null
        private var connection = ""
        private var fingerprint = ""

        fun fail() {
            traffic?.close()
            events.get()?.cancel()
            failed.complete(Unit)
        }

        suspend fun connect(iceRestart: Boolean) {
            val configuration =
                PeerConnection.RTCConfiguration(emptyList()).apply {
                    tcpCandidatePolicy = PeerConnection.TcpCandidatePolicy.DISABLED
                    continualGatheringPolicy =
                        PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
                }
            factory = PeerConnectionFactory.builder().createPeerConnectionFactory()
            peer =
                factory!!.createPeerConnection(
                    configuration,
                    object : PeerConnection.Observer {
                        override fun onSignalingChange(state: PeerConnection.SignalingState) {}

                        override fun onIceConnectionChange(
                            state: PeerConnection.IceConnectionState
                        ) {
                            if (
                                state in
                                    setOf(
                                        PeerConnection.IceConnectionState.FAILED,
                                        PeerConnection.IceConnectionState.DISCONNECTED,
                                        PeerConnection.IceConnectionState.CLOSED,
                                    )
                            )
                                fail()
                        }

                        override fun onIceConnectionReceivingChange(receiving: Boolean) {}

                        override fun onIceGatheringChange(
                            state: PeerConnection.IceGatheringState
                        ) {}

                        override fun onIceCandidate(candidate: IceCandidate) {
                            val value = buildJsonObject {
                                put("kind", "candidate")
                                put("candidate", candidate.sdp)
                                put("sdp_mid", candidate.sdpMid)
                                put("sdp_m_line_index", candidate.sdpMLineIndex)
                            }
                            if (!candidates.trySend(value).isSuccess) fail()
                        }

                        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) {}

                        override fun onAddStream(stream: MediaStream) {
                            fail()
                        }

                        override fun onRemoveStream(stream: MediaStream) {}

                        override fun onDataChannel(channel: DataChannel) {
                            fail()
                        }

                        override fun onRenegotiationNeeded() {}
                    },
                ) ?: throw IOException("Pair direct indisponible")
            data = peer!!.createDataChannel("leo.v4", DataChannel.Init())
            traffic =
                DirectChannel(
                    { packet ->
                        val channel = data ?: throw IOException("Canal direct fermé")
                        if (
                            channel.state() != DataChannel.State.OPEN ||
                                channel.bufferedAmount() + packet.size > DirectChannel.MAX_FRAME ||
                                !channel.send(DataChannel.Buffer(ByteBuffer.wrap(packet), true))
                        )
                            throw IOException("Canal direct saturé")
                    },
                    { failed.complete(Unit) },
                    {
                        !stopped.get() &&
                            !failed.isCompleted &&
                            System.currentTimeMillis() < expires.get()
                    },
                )
            data!!.registerObserver(
                object : DataChannel.Observer {
                    override fun onBufferedAmountChange(previous: Long) {}

                    override fun onStateChange() {
                        when (data?.state()) {
                            DataChannel.State.OPEN -> {
                                if (
                                    !failed.isCompleted &&
                                        System.currentTimeMillis() < expires.get()
                                ) {
                                    api.transport.attach(traffic!!)
                                    opened.complete(Unit)
                                } else fail()
                            }
                            DataChannel.State.CLOSED,
                            DataChannel.State.CLOSING -> fail()
                            else -> {}
                        }
                    }

                    override fun onMessage(buffer: DataChannel.Buffer) {
                        val size = buffer.data.remaining()
                        if (
                            !buffer.binary ||
                                size !in DirectChannel.HEADER..DirectChannel.MAX_PACKET
                        ) {
                            fail()
                            return
                        }
                        // Native frees the ByteBuffer after this callback; copy before queuing.
                        val packet = ByteArray(size)
                        buffer.data.get(packet)
                        if (!packets.trySend(packet).isSuccess) fail()
                    }
                }
            )
            jobs += scope.launch {
                try {
                    for (packet in packets) traffic!!.receive(packet)
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    fail()
                }
            }
            if (iceRestart) peer!!.restartIce()
            val constraints =
                MediaConstraints().apply {
                    if (iceRestart)
                        mandatory.add(MediaConstraints.KeyValuePair("IceRestart", "true"))
                }
            val offer = requireNotNull(description { peer!!.createOffer(it, constraints) })
            val sdp =
                offer.description
                    .lineSequence()
                    .filter { it.isNotEmpty() }
                    .joinToString("\r\n", postfix = "\r\n") { line ->
                        when {
                            line.startsWith("a=fingerprint:sha-256 ") ->
                                "a=fingerprint:sha-256 " +
                                    line.substringAfter("sha-256 ").uppercase()
                            // v4 negotiates trickle only. The optional SDK extension needs
                            // both peers' support; do not advertise it to the delivered verifier.
                            line == "a=ice-options:trickle renomination" -> "a=ice-options:trickle"
                            else -> line
                        }
                    }
            fingerprint =
                sdp.lineSequence()
                    .first { it.startsWith("a=fingerprint:") }
                    .removePrefix("a=fingerprint:")
            val grant = authorize("authorize")
            if (grant["available"]?.jsonPrimitive?.booleanOrNull != true)
                throw IOException("Direct indisponible")
            connection =
                grant["grant"]!!
                    .jsonObject["claims"]!!
                    .jsonObject["connection_id"]!!
                    .jsonPrimitive
                    .content
            updateGrant(grant)
            configuration.iceServers =
                grant["iceServers"]!!.jsonArray.flatMap { server ->
                    server.jsonObject["urls"]!!.jsonArray.map { url ->
                        val address = url.jsonPrimitive.content
                        require(address.startsWith("stun:") && address.length <= 2048)
                        PeerConnection.IceServer.builder(address).createIceServer()
                    }
                }
            if (!peer!!.setConfiguration(configuration))
                throw IOException("Configuration ICE refusée")
            jobs += scope.launch { readSignals() }
            // Acknowledge the offer before native trickle starts: the installation creates its peer
            // here.
            signal(
                buildJsonObject {
                    put("kind", "offer")
                    put("sdp", sdp)
                }
            )
            description {
                peer!!.setLocalDescription(
                    it,
                    SessionDescription(SessionDescription.Type.OFFER, sdp),
                )
            }
            jobs += scope.launch {
                try {
                    for (candidate in candidates) signal(candidate)
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    fail()
                }
            }
            jobs += scope.launch {
                while (isActive && !failed.isCompleted) {
                    if (System.currentTimeMillis() >= expires.get()) {
                        fail()
                        break
                    }
                    traffic!!.checkDeadline()
                    delay(100)
                }
            }
            jobs += scope.launch {
                try {
                    while (isActive && !failed.isCompleted) {
                        delay(maxOf(1, expires.get() - System.currentTimeMillis() - 30000))
                        updateGrant(authorize("$connection/renew"))
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    fail()
                }
            }
            withTimeoutOrNull(30000) {
                select<Unit> {
                    opened.onAwait {}
                    failed.onAwait {}
                }
            } ?: fail()
        }

        private suspend fun authorize(path: String): JsonObject =
            wireJson
                .parseToJsonElement(
                    api.request(
                        "POST",
                        "/installations/${segment(api.installationId!!)}/direct/$path",
                        buildJsonObject {
                            put("fingerprint", fingerprint)
                            put("versions", buildJsonArray { add(4) })
                        },
                    )
                )
                .jsonObject

        private fun updateGrant(grant: JsonObject) {
            require(grant["available"]!!.jsonPrimitive.boolean)
            val claims = grant["grant"]!!.jsonObject["claims"]!!.jsonObject
            require(
                claims["connection_id"]!!.jsonPrimitive.content == connection &&
                    claims["installation_id"]!!.jsonPrimitive.content == api.installationId &&
                    claims["fingerprint"]!!.jsonPrimitive.content == fingerprint
            )
            val deadline = claims["expires_at"]!!.jsonPrimitive.long * 1000
            require(deadline > System.currentTimeMillis())
            expires.set(deadline)
        }

        private suspend fun signal(value: JsonObject) {
            api.request(
                "POST",
                "/installations/${segment(api.installationId!!)}/direct/${segment(connection)}/signal",
                value,
            )
        }

        private suspend fun readSignals() {
            val call =
                api.streaming.newCall(
                    api.builder(
                            "/installations/${segment(api.installationId!!)}/direct/${segment(connection)}/events"
                        )
                        .header("Accept", "text/event-stream")
                        .get()
                        .build()
                )
            events.set(call)
            if (failed.isCompleted) call.cancel()
            try {
                call.execute().use { response ->
                    checkResponse(response)
                    val source = response.body.source()
                    var value = ""
                    var kind = ""
                    var answered = false
                    val waiting = mutableListOf<IceCandidate>()
                    while (!source.exhausted() && !failed.isCompleted) {
                        val line = source.readUtf8LineStrict(16384)
                        when {
                            line.startsWith("event:") -> kind = line.substringAfter(':').trim()
                            line.startsWith("data:") -> {
                                value += line.substringAfter(':').trim()
                                require(value.length <= 16384)
                            }
                            line.isEmpty() -> {
                                if (kind == "signal" && value.isNotEmpty()) {
                                    val incoming = wireJson.parseToJsonElement(value).jsonObject
                                    when (incoming["kind"]!!.jsonPrimitive.content) {
                                        "answer" -> {
                                            require(!answered)
                                            description {
                                                peer!!.setRemoteDescription(
                                                    it,
                                                    SessionDescription(
                                                        SessionDescription.Type.ANSWER,
                                                        incoming["sdp"]!!.jsonPrimitive.content,
                                                    ),
                                                )
                                            }
                                            answered = true
                                            waiting.forEach { require(peer!!.addIceCandidate(it)) }
                                            waiting.clear()
                                        }
                                        "candidate" -> {
                                            val candidate =
                                                IceCandidate(
                                                    incoming["sdp_mid"]!!
                                                        .jsonPrimitive
                                                        .contentOrNull,
                                                    incoming["sdp_m_line_index"]!!
                                                        .jsonPrimitive
                                                        .int,
                                                    incoming["candidate"]!!.jsonPrimitive.content,
                                                )
                                            if (answered) require(peer!!.addIceCandidate(candidate))
                                            else {
                                                require(waiting.size < 16)
                                                waiting.add(candidate)
                                            }
                                        }
                                        else -> throw IOException("Signal direct inattendu")
                                    }
                                }
                                kind = ""
                                value = ""
                            }
                        }
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {} finally {
                fail()
            }
        }

        suspend fun close() {
            fail()
            jobs.forEach { it.cancel() }
            jobs.joinAll()
            candidates.close()
            packets.close()
            data?.unregisterObserver()
            data?.close()
            data?.dispose()
            peer?.close()
            peer?.dispose()
            factory?.dispose()
        }
    }

    private suspend fun description(start: (SdpObserver) -> Unit): SessionDescription? {
        val result = CompletableDeferred<SessionDescription?>()
        start(
            object : SdpObserver {
                override fun onCreateSuccess(description: SessionDescription) {
                    result.complete(description)
                }

                override fun onSetSuccess() {
                    result.complete(null)
                }

                override fun onCreateFailure(error: String) {
                    result.completeExceptionally(IOException("Négociation directe refusée"))
                }

                override fun onSetFailure(error: String) {
                    result.completeExceptionally(IOException("Négociation directe refusée"))
                }
            }
        )
        return result.await()
    }
}
