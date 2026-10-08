package dev.leo.manager.data

import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Cookie
import okhttp3.CookieJar
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

val wireJson = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
    explicitNulls = false
}

class ApiException(val status: Int, message: String) : IOException(message)

fun serverOrigin(input: String, allowLocalHttp: Boolean = false): HttpUrl {
    val url = input.trim().toHttpUrl()
    require(
        url.username.isEmpty() &&
            url.password.isEmpty() &&
            url.query == null &&
            url.fragment == null
    ) {
        "Saisissez l’adresse du serveur, sans identifiants ni paramètres."
    }
    require(url.encodedPath == "/") { "Utilisez l’origine du serveur, sans chemin supplémentaire." }
    require(
        url.isHttps || (allowLocalHttp && url.host in setOf("localhost", "127.0.0.1", "10.0.2.2"))
    ) {
        "Une adresse HTTPS est nécessaire."
    }
    return url
}

interface SessionVault {
    fun read(origin: String): String?

    fun write(origin: String, cookie: String?)
}

class SessionCookies(private val origin: HttpUrl, private val vault: SessionVault) : CookieJar {
    private var session: Cookie? = vault.read(origin.toString())?.let { Cookie.parse(origin, it) }
    private val challenges = mutableMapOf<String, Cookie>()

    @Synchronized
    override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) {
        if (url.scheme != origin.scheme || url.host != origin.host || url.port != origin.port)
            return
        cookies
            .filter { it.name in setOf("leo_oauth", "leo_passkey") }
            .forEach {
                if (it.expiresAt > System.currentTimeMillis()) challenges[it.name] = it
                else challenges.remove(it.name)
            }
        cookies
            .lastOrNull { it.name == "leo_session" }
            ?.let {
                session = it.takeIf { cookie -> cookie.expiresAt > System.currentTimeMillis() }
                vault.write(origin.toString(), session?.toString())
            }
    }

    @Synchronized
    override fun loadForRequest(url: HttpUrl): List<Cookie> {
        if (url.scheme != origin.scheme || url.host != origin.host || url.port != origin.port)
            return emptyList()
        return (listOfNotNull(
            session?.takeIf { it.expiresAt > System.currentTimeMillis() && it.matches(url) }
        ) +
            challenges.values.filter {
                it.expiresAt > System.currentTimeMillis() && it.matches(url)
            })
    }

    @Synchronized
    fun clear() {
        session = null
        challenges.clear()
        vault.write(origin.toString(), null)
    }
}

class LeoApi(
    val origin: HttpUrl,
    vault: SessionVault,
    client: OkHttpClient = OkHttpClient(),
    val installationId: String? = null,
) {
    val cacheScope = origin.toString() + installationId.orEmpty()
    val transport = DirectTransport(installationId) { restartSubscriptions() }

    private val cookies = SessionCookies(origin, vault)
    val hasSession: Boolean
        get() = cookies.loadForRequest(origin).any { it.name == "leo_session" }

    internal val http =
        client
            .newBuilder()
            .cookieJar(cookies)
            .addInterceptor(transport)
            .retryOnConnectionFailure(false)
            .followRedirects(false)
            .followSslRedirects(false)
            // Leave a complete relay request deadline after the optional direct attempt.
            .callTimeout(java.time.Duration.ofSeconds(65))
            .build()
    internal val streaming =
        http
            .newBuilder()
            .callTimeout(java.time.Duration.ZERO)
            .readTimeout(java.time.Duration.ofSeconds(45))
            .build()
    private val transfers =
        http
            .newBuilder()
            .callTimeout(java.time.Duration.ofMinutes(10))
            .readTimeout(java.time.Duration.ofSeconds(45))
            .build()
    private val imports =
        http
            .newBuilder()
            .callTimeout(java.time.Duration.ofMinutes(4))
            .readTimeout(java.time.Duration.ofMinutes(4))
            .build()
    internal val streamCalls = java.util.concurrent.ConcurrentHashMap.newKeySet<Call>()
    internal val streamGeneration = java.util.concurrent.atomic.AtomicLong()
    @Volatile var csrf: String = ""

    internal val streamLock = Any()
    private var nativeDirect: NativeDirect? = null

    fun startDirect(context: android.content.Context) =
        synchronized(streamLock) {
            if (installationId != null && nativeDirect == null)
                nativeDirect = NativeDirect(this, context)
        }

    internal fun restartSubscriptions() =
        synchronized(streamLock) { streamCalls.forEach { it.cancel() } }

    fun closeStreams() =
        synchronized(streamLock) {
            nativeDirect?.close()
            nativeDirect = null
            transport.close()
            streamGeneration.incrementAndGet()
            streamCalls.forEach { it.cancel() }
            streamCalls.clear()
        }

    fun clearSession() {
        closeStreams()
        csrf = ""
        cookies.clear()
    }

    internal fun url(path: String): HttpUrl {
        require(path.startsWith("/") && !path.startsWith("//"))
        val route = path.substringBefore('?')
        val accountPath =
            route.startsWith("/account/") ||
                route == "/installations" ||
                route.startsWith("/installations/") ||
                route.startsWith("/mcp/oauth/")
        val prefix =
            when {
                installationId == null || accountPath -> "/api"
                route == "/tokens" || route.startsWith("/tokens/") ->
                    "/api/installations/${segment(installationId)}"
                else -> "/api/installations/${segment(installationId)}/api"
            }
        return origin
            .newBuilder()
            .encodedPath(prefix + route)
            .encodedQuery(path.substringAfter('?', "").ifEmpty { null })
            .build()
    }

    internal fun builder(path: String) =
        Request.Builder()
            .url(url(path))
            .header("Origin", origin.toString().removeSuffix("/"))
            .header("X-CSRF-Token", csrf)

    suspend inline fun <reified T> get(path: String): T =
        wireJson.decodeFromString(request("GET", path))

    suspend inline fun <reified T> send(
        method: String,
        path: String,
        body: JsonElement? = null,
    ): T = wireJson.decodeFromString(request(method, path, body))

    suspend fun request(method: String, path: String, body: JsonElement? = null): String {
        val payload =
            if (method in setOf("GET", "HEAD")) null
            else (body?.toString() ?: "{}").toRequestBody("application/json".toMediaType())
        return exchange(
            builder(path).header("Accept", "application/json").method(method, payload).build(),
            if (method == "POST" && path == "/projects/github") imports else http,
        ) {
            checkResponse(it)
            it.body.string()
        }
    }

    suspend fun upload(path: String, file: java.io.File): ChatAttachment {
        require(file.length() <= 10L * 1024 * 1024) {
            "Chaque pièce jointe doit faire au maximum 10 Mo."
        }
        val request =
            builder(path).put(file.asRequestBody("application/octet-stream".toMediaType())).build()
        return exchange(request, transfers) { response ->
            checkResponse(response)
            wireJson.decodeFromString<ChatAttachment>(response.body.string())
        }
    }

    suspend fun agentPortrait(id: String, revision: String): ByteArray =
        exchange(builder("/agents/${segment(id)}/avatar?v=${segment(revision)}").get().build()) {
            response ->
            checkResponse(response)
            val bytes = response.body.byteStream().use { readPortraitBytes(it, 512 * 1024) }
            require(bytes.size <= 512 * 1024) { "Le portrait est trop volumineux." }
            bytes
        }

    suspend fun uploadAgentPortrait(id: String, bytes: ByteArray): Agent {
        require(bytes.size <= 5 * 1024 * 1024) { "Choisissez une image de moins de 5 Mo." }
        return exchange(
            builder("/agents/${segment(id)}/avatar")
                .put(bytes.toRequestBody("application/octet-stream".toMediaType()))
                .build()
        ) { response ->
            checkResponse(response)
            wireJson.decodeFromString<Agent>(response.body.string())
        }
    }

    suspend fun download(
        path: String,
        destination: java.io.File,
        maxBytes: Long = 512L * 1024 * 1024,
    ): java.io.File {
        try {
            return exchange(builder(path).get().build(), transfers) { response ->
                checkResponse(response)
                require(response.body.contentLength() <= maxBytes) {
                    "Ce fichier dépasse la limite de téléchargement."
                }
                response.body.byteStream().use { input ->
                    destination.outputStream().use { output ->
                        val buffer = ByteArray(65536)
                        var total = 0L
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            total += count
                            require(total <= maxBytes) {
                                "Ce fichier dépasse la limite de téléchargement."
                            }
                            output.write(buffer, 0, count)
                        }
                    }
                }
                destination
            }
        } catch (e: Exception) {
            destination.delete()
            throw e
        }
    }

    private suspend fun <T> exchange(
        request: Request,
        client: OkHttpClient = http,
        consume: (Response) -> T,
    ): T = suspendCancellableCoroutine { continuation ->
        val call = client.newCall(request)
        continuation.invokeOnCancellation { call.cancel() }
        call.enqueue(
            object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (continuation.isActive) continuation.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    try {
                        val result = response.use(consume)
                        if (continuation.isActive) continuation.resume(result)
                    } catch (e: Exception) {
                        if (continuation.isActive) continuation.resumeWithException(e)
                    }
                }
            }
        )
    }
}

internal fun checkResponse(response: Response) {
    if (response.isSuccessful) return
    val text = response.body.string()
    val message =
        runCatching {
            wireJson.parseToJsonElement(text).jsonObject["error"]?.jsonPrimitive?.content
        }
            .getOrNull() ?: "Le serveur a répondu avec le code ${response.code}."
    throw ApiException(response.code, message)
}

internal fun readPortraitBytes(input: java.io.InputStream, maxBytes: Int): ByteArray {
    val output = java.io.ByteArrayOutputStream()
    val buffer = ByteArray(8192)
    while (true) {
        val count = input.read(buffer)
        if (count < 0) break
        require(output.size() + count <= maxBytes) { "L’image est trop volumineuse." }
        output.write(buffer, 0, count)
    }
    return output.toByteArray()
}
