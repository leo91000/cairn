package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class AntigravityAccountsTest {
    @Test
    fun `Google Cloud credentials, project and region become one JSON key`() {
        val key =
            Json.parseToJsonElement(
                    googleCloudKey(
                        """{"type":"authorized_user","client_id":"id"}""",
                        " cairn-test ",
                        "europe-west1",
                    )
                )
                .jsonObject
        assertEquals(
            "authorized_user",
            key.getValue("credentials").jsonObject.getValue("type").jsonPrimitive.content,
        )
        assertEquals("cairn-test", key.getValue("project").jsonPrimitive.content)
        assertEquals("europe-west1", key.getValue("location").jsonPrimitive.content)
    }

    @Test
    fun `blank project and region are left to the server`() {
        val key = Json.parseToJsonElement(googleCloudKey("""{"type":"x"}""", " ", "")).jsonObject
        assertEquals(setOf("credentials"), key.keys)
    }

    @Test
    fun `credentials must be a JSON object`() {
        assertNull(googleCloudCredentialsError(""))
        assertNull(googleCloudCredentialsError("""{"type":"service_account"}"""))
        assertEquals("Ce fichier n’est pas un JSON valide.", googleCloudCredentialsError("{"))
        assertEquals(
            "Le fichier d’identifiants doit contenir un objet JSON.",
            googleCloudCredentialsError("[1]"),
        )
        assertThrows(IllegalArgumentException::class.java) { googleCloudKey("[1]", "", "") }
    }

    @Test
    fun `only Antigravity Google accounts sign in again; key accounts replace their key`() {
        fun account(provider: String, modelProvider: String?) =
            Account(provider = provider, modelProvider = modelProvider)
        assertTrue(signsIn(account("antigravity", "google")))
        // An account still signing in has no model provider yet.
        assertTrue(signsIn(account("antigravity", null)))
        assertTrue(replacesKey(account("antigravity", "gemini")))
        assertTrue(replacesKey(account("antigravity", "google-cloud")))
        // OpenCode can connect a Google model-provider key: it stays a key.
        assertTrue(replacesKey(account("opencode", "google")))
        assertTrue(signsIn(account("claude", null)))
        assertTrue(signsIn(account("codex", null)))
    }

    @Test
    fun `Antigravity is an ordered key agent with plan turns`() {
        val agent = codingAgent("antigravity")
        assertEquals("Antigravity", agent.label)
        assertTrue(agent.apiKeys && agent.accountSignIn && agent.ordered && agent.planTurns)
        assertTrue(agent.requiresModel)
        assertFalse(codingAgent("opencode").planTurns)
    }
}
