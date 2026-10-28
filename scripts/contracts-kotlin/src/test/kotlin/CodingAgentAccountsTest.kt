import build.cairn.contracts.AccountSettings
import build.cairn.contracts.AccountSignIn
import build.cairn.contracts.CodingAgentAccount
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlinx.serialization.json.*

class CodingAgentAccountsTest {
    @Test
    fun sharedAccountCorpusPreservesDecimalsUnknownUsageAndWireIntegers() {
        val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
        val corpus = json.parseToJsonElement(File(System.getProperty("cairn.codingAgentAccountFixtures")).readText()).jsonObject
        val accounts = corpus.getValue("accounts").jsonArray.map { json.decodeFromJsonElement<CodingAgentAccount>(it) }
        assertEquals(12.5, accounts.first().usage!!.windows.first().usedPercent)
        assertEquals(9007199254740991L, accounts[1].maxConcurrentRuns)
        assertEquals(null, accounts[1].usage)
        assertEquals(true, accounts[2].usage!!.useBalance)
        for (account in accounts) assertEquals(account, json.decodeFromString<CodingAgentAccount>(json.encodeToString(account)))
        for (input in corpus.getValue("settings").jsonArray) json.decodeFromJsonElement<AccountSettings>(input)
        for (input in corpus.getValue("invalidSettings").jsonArray.take(3)) assertFails { json.decodeFromJsonElement<AccountSettings>(input) }
        json.decodeFromJsonElement<AccountSignIn>(corpus.getValue("signIn"))
    }
}
