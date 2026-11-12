import build.cairn.contracts.CreateHookRuleInput
import build.cairn.contracts.RuleAction
import build.cairn.contracts.RuleCondition
import build.cairn.contracts.RuleNode
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlin.test.assertIs
import kotlinx.serialization.json.*

class HookRulesTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun nestedStepsAndConditionsDecodeOnDemandAndRoundTrip() {
        val wire =
            json.parseToJsonElement(
                """
                {
                    "definition":{
                        "name":"Support",
                        "description":"",
                        "trigger":{"type":"plugin","instanceId":"00000000-0000-4000-8000-000000000011","hook":"message"},
                        "executionAgentId":"00000000-0000-4000-8000-000000000010",
                        "steps":[{
                            "type":"if",
                            "id":"known",
                            "condition":{"type":"all","conditions":[
                                {"type":"authorIn","author":{"type":"formula","expression":"payload.author"},"people":[{"resourceType":"person","id":"alice"}]},
                                {"type":"not","condition":{"type":"formula","expression":"payload.bot"}}
                            ]},
                            "then":[{
                                "type":"action","id":"post",
                                "action":{"type":"callTool","instanceId":"00000000-0000-4000-8000-000000000011","tool":"post","input":{"text":{"type":"template","text":"Hi {{payload.author}}"}}}
                            }],
                            "otherwise":[]
                        }],
                        "limits":{"triggeringsPerHour":60,"conversationsPerHour":10}
                    },
                    "enabled":true
                }
                """
                    .trimIndent()
            )
        val decoded = json.decodeFromJsonElement<CreateHookRuleInput>(wire)
        val branch = assertIs<RuleNode.A>(decoded.definition.steps.single())
        val group = assertIs<RuleCondition.A>(branch.condition)
        assertIs<RuleCondition.F>(json.decodeFromJsonElement<RuleCondition>(group.conditions[0]))
        val action =
            assertIs<RuleNode.C>(json.decodeFromJsonElement<RuleNode>(branch.then.single()))
        assertIs<RuleAction.A>(action.action)
        assertEquals(wire, json.encodeToJsonElement(decoded))
        val invalid =
            wire.toString().replace("\"forEach\"", "\"while\"").replace("\"if\"", "\"while\"")
        assertFails { json.decodeFromString<CreateHookRuleInput>(invalid) }
    }
}
