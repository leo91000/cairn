package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleLimits
import build.cairn.contracts.RuleCatalogAgent
import build.cairn.contracts.RuleCatalogLimits
import build.cairn.contracts.RuleCatalogTool
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class RulesCreationCases {
    @get:Rule val compose = createComposeRule()

    private fun rootSchemaCatalog(): HookRuleCatalog {
        val schema =
            wireJson.parseToJsonElement(
                """{"type":"object","${'$'}ref":"#/${'$'}defs/Input","${'$'}defs":{"Input":{"type":"object","required":["text"],"properties":{"text":{"type":"string","title":"Message","minLength":1}}}}}"""
            )
        return HookRuleCatalog(
            actions = emptyList(),
            agents = emptyList(),
            conditions = emptyList(),
            functions = emptyList(),
            hooks = emptyList(),
            limits =
                RuleCatalogLimits(
                    defaultLimits =
                        HookRuleLimits(conversationsPerHour = 5, triggeringsPerHour = 60),
                    maxConversationsPerHour = 10,
                    maxChainDepth = 8,
                    maxConversationValuesBytes = 262144,
                    maxNodes = 100,
                    maxSteps = 500,
                    maxToolCalls = 100,
                    maxTriggeringsPerHour = 100,
                    maxWaitSeconds = 604800,
                ),
            operators = emptyList(),
            tools =
                listOf(
                    RuleCatalogTool(
                        agentIds = listOf(MAIN_AGENT_ID),
                        description = "",
                        inputSchema = schema,
                        instanceId = "00000000-0000-4000-8000-000000000015",
                        instanceName = "Root schema",
                        pluginId = "root",
                        resourceFields = emptyList(),
                        tool = "root_post",
                    )
                ),
            variables = emptyList(),
        )
    }

    @Test
    fun aRootToolReferenceInitializesRequiredInputs() {
        val catalog = rootSchemaCatalog()
        val definition =
            ruleObject(
                "executionAgentId" to JsonPrimitive(MAIN_AGENT_ID),
                "steps" to JsonArray(emptyList()),
            )
        val step = newRuleStep(definition, "callTool", catalog)
        assertTrue(step.obj("action").obj("input")!!.containsKey("text"))
    }

    @Test
    fun aRootToolReferenceShowsItsEditableFields() {
        val catalog = rootSchemaCatalog()
        var node by
            mutableStateOf(
                ruleObject(
                    "type" to JsonPrimitive("action"),
                    "id" to JsonPrimitive("post"),
                    "action" to
                        ruleObject(
                            "type" to JsonPrimitive("callTool"),
                            "instanceId" to JsonPrimitive(catalog.tools[0].instanceId),
                            "tool" to JsonPrimitive("root_post"),
                            "input" to ruleObject("text" to templateValue()),
                        ),
                )
            )
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                },
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("fixture", vm) }
        try {
            compose.setContent {
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleStepPanel(
                            vm,
                            Workspace(),
                            node,
                            catalog,
                            MAIN_AGENT_ID,
                            emptyList(),
                            null,
                        ) {
                            node = it
                        }
                    }
                }
            }
            compose.onNodeWithText("Message").assertIsDisplayed()
            compose.onAllNodes(hasSetTextAction()).assertCountEquals(2)
            compose
                .onAllNodes(hasSetTextAction())
                .onLast()
                .performScrollTo()
                .performTextReplacement("Hello")
            compose.runOnIdle {
                assertEquals(templateValue("Hello"), node.obj("action").obj("input").obj("text"))
            }
        } finally {
            compose.runOnIdle { models.clear() }
        }
    }

    @Test
    fun agentScopeUsesServerRelationshipsAndClearingReloadsAllRules() {
        val target = "00000000-0000-4000-8000-000000000099"
        val requests = CopyOnWriteArrayList<String>()
        fun response(body: String) =
            MockResponse().setHeader("Content-Type", "application/json").setBody(body)
        val directed =
            """{"id":"directed","name":"Conversation par clé","executionAgentId":"$MAIN_AGENT_ID","startedAgentIds":[],"toolInstanceIds":[],"creatorAccountId":"owner","version":1,"blocks":[],"trigger":{"type":"cairn","hook":"replyReady"}}"""
        val unrelated =
            directed.replace("directed", "unrelated").replace("Conversation par clé", "Autre règle")
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val url = request.requestUrl!!
                        if (url.encodedPath.endsWith("/hook-rules")) {
                            requests += url.encodedQuery.orEmpty()
                            return response(
                                if (url.queryParameter("agentId") == target)
                                    """{"rules":[$directed]}"""
                                else """{"rules":[$directed,$unrelated]}"""
                            )
                        }
                        if (url.encodedPath.endsWith("/hook-rules/catalog"))
                            return response(wireJson.encodeToString(rootSchemaCatalog()))
                        return response(
                            when {
                                url.encodedPath == "/api/account/session" -> beaconAccountFixture()
                                url.encodedPath == "/api/installations" ->
                                    beaconInstallationsFixture()
                                url.encodedPath.endsWith("/api/projects") -> """{"projects":[]}"""
                                url.encodedPath.endsWith("/api/agents") -> """{"agents":[]}"""
                                url.encodedPath.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                url.encodedPath.endsWith("/api/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        )
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.setContent {
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(origin)
                    }
                    val state by vm.state.collectAsState()
                    CairnTheme {
                        if (state.installation != null)
                            RulesScreen(vm, state, agentFilter = target, edit = {})
                    }
                }
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Conversation par clé")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertTrue(requests.contains("agentId=$target"))
                compose.onNodeWithText("Autre règle").assertDoesNotExist()
                compose.onNodeWithText("Afficher toutes les règles").performClick()
                compose.waitUntil(20000) {
                    compose.onAllNodesWithText("Autre règle").fetchSemanticsNodes().isNotEmpty()
                }
                assertTrue(requests.contains(""))
            } finally {
                compose.runOnIdle { models.clear() }
            }
        }
    }

    @Test
    fun aSenderContactHookOffersPeopleByNameWithTheExecutionAgent() {
        val hook =
            wireJson.decodeFromString<build.cairn.contracts.RuleCatalogHook>(
                """{"trigger":{"type":"plugin","instanceId":"00000000-0000-4000-8000-000000000015","hook":"message"},"pluginId":"contacts","title":"Message","description":"","payloadSchema":{"type":"object","properties":{"sender":{"type":"string"}}},"resourceFields":[{"path":"/sender","resourceType":"contact"}],"agentIds":["$MAIN_AGENT_ID"]}"""
            )
        val catalog = rootSchemaCatalog().copy(hooks = listOf(hook))
        val definition =
            ruleObject(
                "trigger" to wireJson.encodeToJsonElement(hook.trigger),
                "executionAgentId" to JsonPrimitive(MAIN_AGENT_ID),
                "steps" to JsonArray(emptyList()),
            )
        val source = rulePeopleSource(hook, definition)
        assertEquals(
            RuleResourceSource(
                "contacts",
                "00000000-0000-4000-8000-000000000015",
                "contact",
                MAIN_AGENT_ID,
            ),
            source,
        )
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/agents") -> """{"agents":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/api/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var condition by
                mutableStateOf(
                    ruleObject(
                        "type" to JsonPrimitive("authorIn"),
                        "author" to formulaRuleValue("payload.sender"),
                        "people" to JsonArray(emptyList()),
                    )
                )
            try {
                compose.setContent {
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(origin)
                    }
                    val state by vm.state.collectAsState()
                    CairnTheme {
                        if (state.installation != null)
                            Column(Modifier.verticalScroll(rememberScrollState())) {
                                RuleConditionEditor(
                                    condition,
                                    ruleVariablesAt(definition, "/steps/0", catalog),
                                    { condition = it },
                                    vm,
                                    source,
                                )
                            }
                    }
                }
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Choisir une personne…")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("Choisir une personne…")
                    .performScrollTo()
                    .assertIsDisplayed()
            } finally {
                compose.runOnIdle { models.clear() }
            }
        }
    }

    @Test
    fun startingAnAgentEditsAndClearsItsExternalAuthor() {
        val catalog =
            rootSchemaCatalog()
                .copy(agents = listOf(RuleCatalogAgent(MAIN_AGENT_ID, "Assistant", false)))
        var node by
            mutableStateOf(
                ruleObject(
                    "type" to JsonPrimitive("action"),
                    "id" to JsonPrimitive("start"),
                    "action" to
                        ruleObject(
                            "type" to JsonPrimitive("startAgent"),
                            "agentId" to JsonPrimitive(MAIN_AGENT_ID),
                            "prompt" to templateValue("Answer this message."),
                        ),
                )
            )
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                },
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("fixture", vm) }
        try {
            compose.setContent {
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleStepPanel(
                            vm,
                            Workspace(),
                            node,
                            catalog,
                            MAIN_AGENT_ID,
                            emptyList(),
                            RuleResourceSource("root", "support", "person", MAIN_AGENT_ID),
                        ) {
                            node = it
                        }
                    }
                }
            }
            compose
                .onNodeWithContentDescription("Auteur externe (facultatif)")
                .performScrollTo()
                .performTextReplacement("payload.author")
            compose.runOnIdle {
                assertEquals(
                    ruleObject(
                        "type" to JsonPrimitive("formula"),
                        "expression" to JsonPrimitive("payload.author"),
                    ),
                    node.obj("action").obj("author"),
                )
                assertTrue(
                    describeAction(node["action"], catalog) { "Assistant" }
                        .second
                        .contains("auteur ƒ payload.author")
                )
            }
            compose
                .onNodeWithContentDescription("Auteur externe (facultatif)")
                .performScrollTo()
                .performTextReplacement("")
            compose.runOnIdle { assertNull(node.obj("action").obj("author")) }
        } finally {
            compose.runOnIdle { models.clear() }
        }
    }

    @Test fun theFirstNumericFieldStartsWithANumericOperand() = addTypedComparison("integer")

    @Test fun theFirstBooleanFieldStartsWithABooleanOperand() = addTypedComparison("boolean")

    private fun addTypedComparison(type: String) {
        var value by
            mutableStateOf(
                ruleObject("type" to JsonPrimitive("all"), "conditions" to JsonArray(emptyList()))
            )
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                },
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("fixture", vm) }
        try {
            compose.setContent {
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleConditionEditor(
                            value,
                            listOf(
                                RuleVariable(
                                    "Événement",
                                    "payload.first",
                                    "Premier champ",
                                    type,
                                    "",
                                )
                            ),
                            { value = it },
                            vm,
                            null,
                        )
                    }
                }
            }
            compose.onNodeWithText("+ Ajouter une condition").performClick()
            compose.onNodeWithText("Comparer un champ").performClick()
            compose.runOnIdle {
                assertEquals("literal", value.list("conditions")[0].obj("right").text("type"))
            }
            if (type == "boolean") compose.onNodeWithText("Oui").performClick()
            else
                compose
                    .onNode(hasSetTextAction() and hasContentDescription("Comparer avec"))
                    .performTextReplacement("5")
            compose.runOnIdle {
                assertEquals(
                    literalRuleValue(
                        if (type == "boolean") JsonPrimitive(true) else JsonPrimitive(5)
                    ),
                    value.list("conditions")[0].obj("right"),
                )
            }
        } finally {
            compose.runOnIdle { models.clear() }
        }
    }

    @Test fun aLostCreationResponseKeepsTheLaterDraft() = recoverCreation(false)

    @Test fun aRecoveredCreationKeepsItsIdentityAcrossRecreation() = recoverCreation(true)

    @Test fun inversionsCannotPushAFullSubtreePastSixLevels() = conditionDepthControls(false)

    @Test
    fun aNegatedGroupCountsBothLevelsBeforeOfferingAnotherGroup() = conditionDepthControls(true)

    private fun conditionDepthControls(negated: Boolean) {
        var inner =
            ruleObject("type" to JsonPrimitive("formula"), "expression" to JsonPrimitive("true"))
        repeat(if (negated) 3 else 4) {
            inner =
                ruleObject("type" to JsonPrimitive("all"), "conditions" to JsonArray(listOf(inner)))
        }
        var value by
            mutableStateOf(
                ruleObject(
                    "type" to JsonPrimitive("all"),
                    "conditions" to
                        JsonArray(
                            listOf(
                                if (negated)
                                    ruleObject("type" to JsonPrimitive("not"), "condition" to inner)
                                else inner
                            )
                        ),
                )
            )
        assertEquals(6, ruleConditionDepth(value))
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                },
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("fixture", vm) }
        try {
            compose.setContent {
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleConditionEditor(value, emptyList(), { value = it }, vm, null)
                    }
                }
            }
            val inversions = compose.onAllNodesWithText("Inverser")
            assertTrue(inversions.fetchSemanticsNodes().isNotEmpty())
            for (i in inversions.fetchSemanticsNodes().indices) inversions[i].assertIsNotEnabled()
            if (negated) {
                compose
                    .onAllNodesWithText("+ Ajouter une condition")[0]
                    .performScrollTo()
                    .performClick()
                compose.onNodeWithText("Groupe ET").assertDoesNotExist()
                compose.onNodeWithText("Groupe OU").assertDoesNotExist()
            }
        } finally {
            compose.runOnIdle { models.clear() }
        }
    }

    @Test fun aKnownCreationRejectionAllowsCorrectingTheDraft() = recoverCreation(false, true)

    @Test
    fun aLostReplacementResponseDoesNotWriteAnotherVersion() =
        recoverCreation(false, replace = true)

    private fun recoverCreation(
        restore: Boolean,
        reject: Boolean = false,
        replace: Boolean = false,
    ) {
        val id = "da2dd0ad-91ce-4566-b426-77c03628f59e"
        val unavailable = AtomicBoolean(true)
        val failureMessage =
            if (reject) "Creation rejected"
            else if (replace) "Uncertain replacement" else "Uncertain creation"
        val closed = AtomicBoolean(false)
        val stored = AtomicReference<JsonObject?>(null)
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        fun record(definition: JsonObject, version: Int) = buildJsonObject {
            put("id", id)
            put("revision", "revision-$version")
            put("version", version)
            put("definition", definition)
            put("allowed", buildJsonObject { put("edit", true) })
        }
        if (replace) {
            val initial =
                wireJson
                    .parseToJsonElement(
                        """{"name":"Original","description":"","trigger":{"type":"cairn","hook":"replyReady"},"executionAgentId":"$MAIN_AGENT_ID","limits":{"triggeringsPerHour":60,"conversationsPerHour":5},"steps":[{"type":"action","id":"save","action":{"type":"storeValue","scope":"rule","name":"last","value":{"type":"literal","value":0}}}]}"""
                    )
                    .jsonObject
            stored.set(record(initial, 1))
        }
        fun json(body: String, status: Int = 200) =
            MockResponse()
                .setResponseCode(status)
                .setHeader("Content-Type", "application/json")
                .setBody(body)
        val catalog =
            """{"agents":[{"agentId":"$MAIN_AGENT_ID","name":"Assistant","native":false}],"hooks":[{"agentIds":["$MAIN_AGENT_ID"],"title":"Réponse prête","description":"","payloadSchema":null,"resourceFields":[],"trigger":{"type":"cairn","hook":"replyReady"}}],"tools":[],"actions":[],"conditions":[],"operators":[],"functions":[],"variables":[],"limits":{"defaultLimits":{"triggeringsPerHour":60,"conversationsPerHour":5},"maxTriggeringsPerHour":100,"maxConversationsPerHour":10,"maxChainDepth":8,"maxConversationValuesBytes":262144,"maxWaitSeconds":604800,"maxSteps":500,"maxToolCalls":100,"maxNodes":100}}"""
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        if (path.endsWith("/hook-rules/validate"))
                            return json("""{"valid":true,"problems":[],"warnings":[]}""")
                        if (path.endsWith("/hook-rules/catalog")) return json(catalog)
                        if (request.method == "POST" && path.endsWith("/hook-rules")) {
                            val body =
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            writes += "POST" to body
                            if (!unavailable.get() || !reject)
                                stored.compareAndSet(null, record(body.obj("definition")!!, 1))
                            return if (unavailable.get())
                                json("""{"error":"$failureMessage"}""", if (reject) 400 else 503)
                            else json(stored.get().toString())
                        }
                        if (path.endsWith("/hook-rules/$id")) {
                            if (request.method == "PUT") {
                                val body =
                                    wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                                writes += "PUT" to body
                                if (body.text("revision") != stored.get().text("revision"))
                                    return json("""{"error":"Revision conflict"}""", 409)
                                stored.set(record(body.obj("definition")!!, 2))
                                if (replace && unavailable.get())
                                    return json("""{"error":"$failureMessage"}""", 503)
                            }
                            return json(stored.get().toString())
                        }
                        return json(
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/agents") -> """{"agents":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/api/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        )
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            val restoration = StateRestorationTester(compose)
            restoration.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                val state by vm.state.collectAsState()
                CairnTheme {
                    if (state.installation != null && !closed.get())
                        RuleEditorScreen(
                            vm,
                            state,
                            if (replace) id else "",
                            saved = { closed.set(true) },
                            back = {},
                        )
                }
            }
            fun waitFor(test: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    test()
                }
            try {
                waitFor {
                    compose.onAllNodesWithText("Nom de la règle").fetchSemanticsNodes().isNotEmpty()
                }
                if (replace) {
                    compose.onNodeWithText("Nom de la règle").performTextReplacement("Latest draft")
                    waitFor {
                        compose
                            .onAllNodes(hasText("Enregistrer") and isEnabled())
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Enregistrer").performClick()
                    waitFor {
                        compose
                            .onAllNodesWithText(failureMessage, substring = true)
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    unavailable.set(false)
                    compose.onNodeWithText("Enregistrer").performClick()
                    waitFor { closed.get() }
                    assertEquals(2, writes.count { it.first == "PUT" })
                    assertEquals(2, stored.get()!!["version"]!!.jsonPrimitive.int)
                    assertEquals("Latest draft", stored.get().obj("definition").text("name"))
                    return
                }
                compose.onNodeWithText("Nom de la règle").performTextReplacement("Original")
                compose.onNodeWithText("Étape").performScrollTo().performClick()
                compose.onNodeWithText("Mémoriser une valeur").performClick()
                compose.onNodeWithText("Terminer cette étape").performScrollTo().performClick()
                waitFor {
                    compose
                        .onAllNodes(hasText("Créer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Créer").performClick()
                waitFor {
                    compose
                        .onAllNodesWithText(failureMessage, substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Nom de la règle").performTextReplacement("Latest draft")
                unavailable.set(false)
                waitFor {
                    compose
                        .onAllNodes(hasText("Créer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Créer").performClick()
                if (reject) {
                    waitFor { closed.get() }
                    assertTrue(writes.none { it.first == "PUT" })
                    assertEquals("Original", writes.first().second.obj("definition").text("name"))
                    assertEquals(
                        "Latest draft",
                        writes.last().second.obj("definition").text("name"),
                    )
                    assertEquals(1, writes.map { it.second.text("creationId") }.toSet().size)
                    assertEquals("Latest draft", stored.get().obj("definition").text("name"))
                    return
                }
                waitFor {
                    compose
                        .onAllNodesWithText(
                            "Vos dernières modifications sont conservées",
                            substring = true,
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertFalse(closed.get())
                compose.onNodeWithText("Nom de la règle").assertTextContains("Latest draft")
                if (restore) {
                    restoration.emulateSavedInstanceStateRestore()
                    waitFor {
                        compose
                            .onAllNodes(hasText("Enregistrer") and isEnabled())
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Enregistrer").performClick()
                } else {
                    compose.onNodeWithText("Enregistrer mon brouillon à la suite").performClick()
                }
                waitFor { closed.get() }
                assertEquals(
                    1,
                    writes
                        .filter { it.first == "POST" }
                        .map { it.second.text("creationId") }
                        .toSet()
                        .size,
                )
                assertTrue(
                    writes
                        .filter { it.first == "POST" }
                        .all { it.second.obj("definition").text("name") == "Original" }
                )
                val update = writes.single { it.first == "PUT" }.second
                assertEquals(id, update.text("ruleId"))
                assertEquals("Latest draft", update.obj("definition").text("name"))
                assertEquals(2, stored.get()!!["version"]!!.jsonPrimitive.int)
            } finally {
                compose.runOnIdle { models.clear() }
            }
        }
    }
}
