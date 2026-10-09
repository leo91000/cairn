package build.cairn.app.data

import kotlinx.coroutines.delay
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import okhttp3.HttpUrl.Companion.toHttpUrl

/** The device credential provider varies; the Cairn account exchange stays on its fixed origin. */
interface CairnCredentials {
    suspend fun google(clientId: String, nonce: String): String

    suspend fun authenticatePasskey(options: String): String

    suspend fun createPasskey(options: String): String

    fun openGitHub(url: String)

    suspend fun clear() {}
}

@Serializable
private data class GoogleChallenge(val challenge: String, val nonce: String, val clientId: String)

@Serializable private data class PasskeyChallenge(val challenge: String, val options: JsonObject)

@Serializable
private data class GitHubHandover(val challenge: String, val secret: String, val url: String)

class CairnAccountSignIn(private val api: CairnApi, private val credentials: CairnCredentials) {
    suspend fun github(): Session {
        val handover =
            api.send<GitHubHandover>(
                "POST",
                "/account/oauth/github/start",
                buildJsonObject { put("native", true) },
            )
        val launcher = handover.url.toHttpUrl()
        require(
            launcher.scheme == api.origin.scheme &&
                launcher.host == api.origin.host &&
                launcher.port == api.origin.port &&
                launcher.username.isEmpty() &&
                launcher.password.isEmpty() &&
                launcher.encodedPath == "/api/account/oauth/github/native/browser"
        ) {
            "Le lien de connexion GitHub est invalide."
        }
        credentials.openGitHub(handover.url)
        return withTimeout(300_000) {
            var result: Session? = null
            while (result == null) {
                val response =
                    wireJson
                        .parseToJsonElement(
                            api.request(
                                "POST",
                                "/account/oauth/github/native/finish",
                                body(
                                    "challenge" to handover.challenge,
                                    "secret" to handover.secret,
                                ),
                            )
                        )
                        .jsonObject
                if (response["pending"]?.jsonPrimitive?.content != "true") {
                    result = acceptSession(wireJson.decodeFromJsonElement<Session>(response))
                }
                if (result == null) delay(1000)
            }
            result
        }
    }

    suspend fun createPasskey(label: String) {
        val challenge = api.send<PasskeyChallenge>("POST", "/account/passkeys/register/start")
        val credential =
            credentials.createPasskey(checkNotNull(challenge.options["publicKey"]).toString())
        api.request(
            "POST",
            "/account/passkeys/register/finish",
            buildJsonObject {
                put("challenge", challenge.challenge)
                put("label", label.trim())
                put("credential", wireJson.parseToJsonElement(credential))
            },
        )
    }

    suspend fun passkey(): Session {
        val challenge = api.send<PasskeyChallenge>("POST", "/account/passkeys/login/start")
        val credential =
            credentials.authenticatePasskey(checkNotNull(challenge.options["publicKey"]).toString())
        val session =
            api.send<Session>(
                "POST",
                "/account/passkeys/login/finish",
                buildJsonObject {
                    put("challenge", challenge.challenge)
                    put("credential", wireJson.parseToJsonElement(credential))
                },
            )
        return acceptSession(session)
    }

    suspend fun confirmPasskey() {
        val challenge = api.send<PasskeyChallenge>("POST", "/account/passkeys/reauth/start")
        val credential =
            credentials.authenticatePasskey(checkNotNull(challenge.options["publicKey"]).toString())
        api.request(
            "POST",
            "/account/passkeys/reauth/finish",
            buildJsonObject {
                put("challenge", challenge.challenge)
                put("credential", wireJson.parseToJsonElement(credential))
            },
        )
    }

    suspend fun google(): Session {
        val challenge =
            api.send<GoogleChallenge>(
                "POST",
                "/account/oauth/google/start",
                buildJsonObject { put("native", true) },
            )
        val token = credentials.google(challenge.clientId, challenge.nonce)
        val session =
            api.send<Session>(
                "POST",
                "/account/oauth/google/callback",
                body("challenge" to challenge.challenge, "credential" to token),
            )
        return acceptSession(session)
    }

    private fun acceptSession(session: Session): Session {
        require(session.authenticated && session.account != null && !session.csrf.isNullOrBlank()) {
            "La session du compte Cairn est invalide."
        }
        api.csrf = session.csrf
        return session
    }
}

@Serializable
data class AccountOptions(
    val google: Boolean = false,
    val github: Boolean = false,
    val passkeys: Boolean = false,
)

@Serializable data class AccountMethod(val id: String, val kind: String, val label: String)

@Serializable data class AccountMethods(val methods: List<AccountMethod> = emptyList())

enum class AccountProvider(val label: String) {
    Google("Google"),
    GitHub("GitHub"),
    Passkey("une passkey"),
}
